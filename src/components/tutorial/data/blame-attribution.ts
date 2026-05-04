/**
 * Blame Attribution Tutorial
 *
 * Informational tutorial explaining how the CI feedback routing system
 * attributes verification failures to specific iterations. Covers the
 * blame engine algorithm, oscillation/revert pattern detection, escalation
 * policies, and cross-run learning integration.
 */

import type { Tutorial } from "../../../types/tutorial";

export const blameAttributionTutorial: Tutorial = {
  id: "blame-attribution",
  title: "Blame Attribution: Why Your Verification Failed",
  description:
    "Understand how the runner traces verification failures back to the exact iteration that caused them. Learn about the blame engine, oscillation detection, escalation policies, and how these systems help the AI converge faster.",
  duration: "12 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["blame", "verification", "convergence", "architecture", "advanced", "featured"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why blame attribution exists and what problem it solves",
    "Know how the blame engine cross-references error output against per-iteration diffs",
    "Understand confidence scoring and what High/Medium/Low mean",
    "Recognize oscillation patterns and revert patterns in the blame data",
    "Know how the escalation policy intervenes when the loop is stuck",
    "Understand how blame feeds into cross-run learning for systemic pattern detection",
    "Know where to find blame data in the UI and API",
  ],
  steps: [
    {
      id: "the-problem",
      title: "The Convergence Problem",
      content: `When a workflow runs, the **verification-agentic loop** iterates: verify the code, find failures, let the AI fix them, verify again. But what happens when the loop *stops converging*?

Consider this scenario:
- **Iteration 1**: AI modifies \`format.ts\` \u2014 verification passes for formatting but a CSS check fails
- **Iteration 2**: AI modifies \`Card.tsx\` to fix the CSS \u2014 but accidentally breaks TypeScript types
- **Iteration 3**: Verification fails on TypeScript compilation. The AI sees the *symptom* but not the *cause*

Without blame attribution, the AI only knows **what** failed. It doesn't know **which of its previous changes** caused the failure. It might waste iterations fixing the wrong file.`,
      estimatedDuration: 1.5,
      tips: [
        "Most agentic loops eventually converge, but stuck loops waste time and tokens",
        "Blame attribution is deterministic \u2014 no LLM calls. It's pure file-path-to-diff cross-referencing",
      ],
    },
    {
      id: "blame-engine-overview",
      title: "The Blame Engine",
      content: `The blame engine is defined in \`blame.rs\` and runs automatically after every failed verification iteration. It produces a **BlameReport** containing:

| Field | Purpose |
|-------|---------|
| \`attributions\` | Per-step: which iteration's changes caused this failure |
| \`oscillating_files\` | Files modified in 3+ consecutive iterations (stuck signal) |
| \`revert_patterns\` | Files with change-skip-change cycles (undone-fix signal) |

The engine is called from \`handle_build_failure_context()\` in the loop handlers, and its output is injected directly into the AI's failure prompt \u2014 so the AI sees exactly which previous change broke what.`,
      estimatedDuration: 1,
      details: `**Key design decisions:**

1. **Deterministic, not LLM-based** \u2014 Blame runs on regex file-path extraction and diff cross-referencing. No API calls, no latency, no cost.

2. **Injected into the prompt, not stored separately** \u2014 The blame context becomes part of the failure string the AI reads. It appears as a "## Failure Attribution" section with per-step blame, confidence, implicated files, and fix suggestions.

3. **Also stored for observability** \u2014 The same BlameReport is serialized to \`blame_json\` on each \`IterationResult\`, emitted as a Tauri event, stored in PG, and recorded as causal events for cross-run learning.`,
    },
    {
      id: "file-path-extraction",
      title: "Step 1: Extract File Paths from Errors",
      content: `The first step of blame attribution is extracting file paths from the verification step's error output (stdout, stderr, error message). The engine uses five regex patterns, each targeting a different compiler/tool format:

| Pattern | Matches |
|---------|---------|
| **Generic** | \`src/components/Card.tsx:47:12\` |
| **Rust** | \`--> src/main.rs:10:5\` |
| **TypeScript** | \`src/types/index.ts(5,1): error TS2305\` |
| **Python** | \`File "src/main.py", line 10\` |
| **Webpack/Vite** | \`ERROR in ./src/App.tsx\` |

The generic pattern recognizes files under common directories (\`src/\`, \`lib/\`, \`config/\`, \`migrations/\`, etc.) with known extensions. All paths are normalized (strip \`./\`, convert backslashes).`,
      estimatedDuration: 1,
      tips: [
        "Extension ordering matters: tsx before ts, jsx before js \u2014 regex alternation takes the first match",
        "The regexes are compiled once via LazyLock statics, not on every call",
        "If no file paths are extractable, the engine falls back to blaming the most recent iteration with low confidence",
      ],
    },
    {
      id: "diff-cross-reference",
      title: "Step 2: Cross-Reference Against Iteration Diffs",
      content: `Each iteration's code changes are captured as an \`IterationDiff\` (via \`compensation::capture_iteration_diff()\`), containing:

- \`files_changed\`: List of modified file paths
- \`diff_stat\`: Human-readable summary (e.g., "3 files changed, 15 insertions, 4 deletions")
- \`diff_summary\`: Truncated unified diff (~4000 chars)
- \`commit_before\` / \`commit_after\`: Git commit hashes for rollback

The blame engine searches these diffs **from most recent to oldest**. For each file path extracted from the error output, it finds the most recent iteration that modified that file. This gives us a mapping: *"error mentioned \`Card.tsx\`, which was last modified in iteration 2."*`,
      estimatedDuration: 1,
      details: `**Path matching logic:**

The engine matches paths using exact comparison or prefix matching:
- \`src/Card.tsx\` matches diff entry \`src/Card.tsx\` (exact)
- \`src/Card.tsx\` matches diff entry \`packages/app/src/Card.tsx\` (diff is a longer prefix)

It does NOT match the reverse (\`Card.tsx\` matching \`src/utils/Card.tsx\`) to avoid false positives from bare filenames.

**Hunk line extraction:**

When available, the engine also parses unified diff hunk headers (\`@@ -N,M +N,M @@\`) to populate \`lines_changed\` on each ImplicatedChange, giving the AI line-level precision.`,
    },
    {
      id: "confidence-scoring",
      title: "Step 3: Confidence Scoring",
      content: `Not all blame attributions are equally certain. The engine assigns a confidence score based on match quality:

| Confidence | Score | Meaning |
|-----------|-------|---------|
| **Very High** | 0.95 | Multiple error files all point to the same iteration |
| **High** | 0.90 | Single file explicitly in error output, matches one iteration |
| **Medium** | 0.60\u20130.70 | Some files matched, some didn't; or ambiguous iteration |
| **Low** | 0.30 | No file paths extractable; blamed most recent iteration by default |

The formatted output uses labels: **High** (\u22650.8), **Medium** (\u22650.5), **Low** (<0.5). The AI is told to prioritize High-confidence attributions and treat Low-confidence ones as hints.`,
      estimatedDuration: 1,
      tips: [
        "The most powerful signal: multiple files from the error output all point to the same iteration. This means the AI's changes in that iteration are almost certainly the cause.",
        "Low-confidence fallback blame is still useful \u2014 it gives the AI a starting point when error messages are generic (e.g., 'Expected 200, got 500')",
      ],
    },
    {
      id: "prompt-injection",
      title: "How Blame Reaches the AI",
      content: `The blame context is injected into the failure prompt that the AI reads before its agentic phase. The injection order in \`handle_build_failure_context()\` is:

1. **Base failure context** \u2014 Which steps passed/failed, stdout/stderr
2. **Convergence analysis** \u2014 Stuck/diverging/oscillating/plateau patterns
3. **Health regression warnings** \u2014 Did the previous iteration make things worse?
4. **Constraint violations** \u2014 Feedback from the constraint engine
5. **Build error enrichment** \u2014 Parsed actionable errors from stderr
6. **Blame attribution** \u2190 *Our new section*
7. **Escalation rethink prompt** \u2014 If stuck too long
8. **Cross-iteration diffs** \u2014 What previous iterations changed

The blame section appears as:
\`\`\`
## Failure Attribution
### CSS layout check (FAILED)
**Blamed:** Iteration 2
**Confidence:** High
**Implicated files:**
- \`src/components/Card.tsx\` (iteration 2)
**Suggestion:** Focus your fix on the implicated file(s)...
\`\`\``,
      estimatedDuration: 1.5,
    },
    {
      id: "oscillation-detection",
      title: "Oscillation Detection",
      content: `One of the most valuable signals from the blame engine: **file oscillation detection**.

When the same file is modified in **3 or more consecutive iterations**, the engine flags it as oscillating. This is a strong signal that the AI is stuck in a loop:

> *Iteration 3: modify Card.tsx*
> *Iteration 4: modify Card.tsx*
> *Iteration 5: modify Card.tsx*
> \u26a0\ufe0f **STOP: File Oscillation Detected**

The formatted warning tells the AI to **stop modifying that file and try a fundamentally different approach**. The \`OscillatingFile\` struct tracks the file path, the number of consecutive iterations (\`consecutive_blames\`), and the specific iteration numbers involved.`,
      estimatedDuration: 1,
      details: `**Implementation detail:**

The engine tracks per-file iteration presence across all accumulated diffs, then scans from the end to find the longest consecutive streak. Only the consecutive tail is reported \u2014 if a file appeared in iterations [1, 3, 4, 5], only [3, 4, 5] is flagged.

**Revert patterns** are a related but distinct signal: a file modified in a non-consecutive alternating pattern (e.g., iterations [1, 3, 5] \u2014 modified, skipped, modified, skipped, modified). This suggests the AI's changes are being undone. Files already flagged as oscillating are excluded from revert detection to avoid duplicate warnings.`,
      tips: [
        "Oscillation is the #1 cause of wasted iterations in stuck loops",
        "The convergence detector (convergence.rs) also tracks oscillation at the step level \u2014 blame adds the file-level dimension",
      ],
    },
    {
      id: "escalation-policy",
      title: "Escalation Policies",
      content: `When blame keeps appearing iteration after iteration, the **EscalationPolicy** kicks in. It's a struct on \`LoopConfig\` with three thresholds:

| Threshold | Default | Action |
|-----------|---------|--------|
| \`warn_after\` | 3 | Emit ESCALATION-WARN log line |
| \`rethink_after\` | 5 | Inject a "step back and rethink" meta-prompt |
| \`time_limit_secs\` | None | Hard-stop the loop (PhaseTimeout) |

The **rethink prompt** is the most interesting: it tells the AI to re-read all failures from scratch, list and challenge every assumption, and try something fundamentally different. It fires at the threshold and then every 3 iterations after (5, 8, 11...) to avoid noise while still reinforcing the message.

The **time limit** is checked in \`CheckPreconditions\` before each iteration starts. If wall-clock time exceeds the limit, the loop terminates with \`CompletionReason::PhaseTimeout\`.`,
      estimatedDuration: 1,
      tips: [
        "Escalation is configurable per-workflow via LoopConfig.escalation_policy",
        "The warn_after threshold just logs \u2014 it's for observability dashboards, not the AI",
        "Rethink fires periodically (not every iteration) because constant nagging degrades AI performance",
      ],
    },
    {
      id: "observability",
      title: "Observability: Events, Logs, and API",
      content: `Blame data flows through four observability channels:

**1. Structured logs** \u2014 \`BLAME iteration=2 step=CSS_check confidence=0.9 files=Card.tsx\`
Also: \`BLAME-OSCILLATION\`, \`BLAME-REVERT\`, \`ESCALATION-WARN\`, \`ESCALATION-RETHINK\`

**2. Tauri events** \u2014 \`AppEvent::BlameAttribution\` emitted via \`EventBroadcaster\` for the frontend dashboard. Contains task_run_id, iteration, counts, and full blame JSON.

**3. Database** \u2014 \`blame_json\` field on \`IterationResult\` (serialized in task run's result_data). Also stored as \`task_run_events\` with type \`"blame_attribution"\` in PG.

**4. MCP API** \u2014 \`GET /task-runs/{id}/blame\` returns aggregated blame data: total attributions, oscillating files, revert patterns, and per-iteration blame reports.`,
      estimatedDuration: 1,
    },
    {
      id: "cross-run-learning",
      title: "Cross-Run Learning Integration",
      content: `Blame attributions are recorded as **causal events** in the SQLite database via \`record_blame_as_causal_events()\`. Each attribution becomes a causal edge:

\`\`\`
cause_type: "iteration_change"    cause_id: "iter-2-change"
effect_type: "verification_failure"  effect_id: "verification-CSS_check"
relationship: "caused_by_change"
confidence: "high"
source: "blame_engine"
\`\`\`

The \`cross_run_learning.rs\` module queries these events to detect **systemic patterns across runs**. If the same file (e.g., \`Card.tsx\`) is blamed across multiple independent workflow runs, that's not a one-off \u2014 it's a structural issue that might need a rule, a constraint, or a refactor.

Oscillation patterns are also stored as causal events (\`cause_type: "file_oscillation"\`, \`effect_type: "convergence_stuck"\`), feeding into the learning system's ability to auto-disable ineffective rules and detect recurring fix patterns.`,
      estimatedDuration: 1.5,
      details: `**Data flow summary:**

\`\`\`
Verification fails
  \u2192 blame.rs: analyze_blame()
    \u2192 extract file paths from stderr/stdout
    \u2192 cross-reference against accumulated_diffs
    \u2192 produce BlameReport
  \u2192 loop_handlers.rs: inject into AI prompt
  \u2192 loop_handlers.rs: emit Tauri event + PG event
  \u2192 loop_handlers.rs: store as causal_events (SQLite)
  \u2192 IterationResult.blame_json (persisted with task run)
  \u2192 GET /task-runs/{id}/blame (queryable via API)
  \u2192 cross_run_learning.rs: detect_recurring_findings() (next run)
\`\`\``,
    },
    {
      id: "convergence-widget",
      title: "The Convergence Widget",
      content: `On the active dashboard, the **Convergence Widget** provides a real-time visual of the loop's progress. It shows a mini bar chart of failed step counts per iteration, with color-coded bars:

- **Green** (\`bg-emerald-500\`): Zero failures \u2014 the iteration passed
- **Amber** (\`bg-amber-500\`): Same or fewer failures than before \u2014 holding steady or improving
- **Red** (\`bg-red-500\`): More failures than before \u2014 regression

A trend indicator (**Improving**, **Stalled**, **Regressing**) gives at-a-glance status. The widget subscribes to the \`iteration-metrics\` Tauri event and updates incrementally as iterations complete.

The blame system's \`blame-attribution\` event is architecturally available to the widget for future enhancements like oscillation overlays on the chart bars.`,
      estimatedDuration: 1,
      tips: [
        "The widget displays at most 20 iterations (MAX_VISIBLE_ITERATIONS) with horizontal scroll",
        "It filters events to the current task run via useCurrentTaskRunId context",
        "The widget returns null before the first iteration completes, avoiding empty-chart noise",
      ],
    },
    {
      id: "mental-model",
      title: "The Mental Model",
      content: `Here's how to think about the blame attribution system:

**The AI is a developer who can't remember what they changed.** After each failed verification, the AI sees the current error output but has lost context about its own previous changes. Blame attribution gives it that context back \u2014 "you changed Card.tsx in iteration 2 and that's why the CSS check is now failing."

**The system forms a feedback tightening loop:**

1. **Blame** narrows the AI's focus to the right files
2. **Oscillation detection** prevents the AI from thrashing on the same file
3. **Escalation** forces the AI to step back when incremental fixes aren't working
4. **Cross-run learning** catches systemic issues that no single run can see

The result: fewer wasted iterations, faster convergence, and better root-cause fixing instead of symptom patching.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "Blame Attribution Spec",
          url: "http://localhost:9876/spec/page/blame-attribution",
          type: "documentation",
        },
        {
          title: "Convergence Widget Spec",
          url: "http://localhost:9876/spec/page/convergence-widget",
          type: "documentation",
        },
      ],
    },
  ],
};
