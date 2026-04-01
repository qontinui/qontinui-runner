/**
 * Adaptive Learning Tutorial (Informational + Operational)
 *
 * Explains the closed-loop learning system that continuously improves
 * verification step generation quality through: playbook lessons from
 * workflow runs, curated few-shot examples, template lifecycle management,
 * GEPA prompt evolution, and PRM retraining triggers. Then walks through
 * the three UI panels to browse, manage, and monitor the system.
 */

import type { Tutorial } from "../../../types/tutorial";

export const adaptiveLearningTutorial: Tutorial = {
  id: "adaptive-learning",
  title: "Adaptive Learning & Prompt Evolution",
  description:
    "Understand how Qontinui continuously improves verification step quality by learning from every workflow run — extracting lessons, curating examples, evolving prompts via GEPA, and managing template lifecycles.",
  duration: "12 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "meta-optimizer",
  category: "Architecture",
  tags: [
    "adaptive-learning",
    "prompt-evolution",
    "gepa",
    "playbook",
    "few-shot",
    "template-lifecycle",
    "featured",
  ],
  prerequisites: ["meta-prompt-optimizer"],
  learningObjectives: [
    "Understand the 5-component learning loop that runs after every workflow",
    "Know how the Reflector extracts lessons from successes and failures",
    "See how the Curator deduplicates and manages playbook entries",
    "Understand the few-shot curation pipeline and quality gates",
    "Know how templates are promoted, demoted, and retired based on performance",
    "Understand GEPA prompt evolution and its Python sidecar architecture",
    "Use the Adaptive Learning dashboard to monitor trends and stats",
    "Manage playbook entries: activate, retire, delete, and inspect source runs",
    "Read GEPA before/after prompt diffs to understand what changed",
  ],
  steps: [
    // ── Informational: Architecture ──────────────────────────────────────
    {
      id: "intro",
      title: "The Learning Loop",
      content: `Every time a workflow run completes, Qontinui's **adaptive learning system** extracts knowledge to improve future runs. This is a closed-loop system — it gets smarter with every execution.

**5 components run after every workflow:**
1. **Reflector** — analyzes what went well or badly
2. **Curator** — deduplicates and manages a playbook of lessons
3. **Few-Shot Curator** — extracts gold-standard examples from first-attempt successes
4. **Template Lifecycle Manager** — promotes/demotes verification templates
5. **GEPA Optimizer** — evolves generation prompts using DSPy (periodic)

The orchestrator also triggers **PRM retraining** after 100 new runs accumulate.`,
      estimatedDuration: 1,
      tips: [
        "All learning is async and non-blocking — it never delays workflow execution",
        "The system degrades gracefully if any component fails",
      ],
    },
    {
      id: "reflector",
      title: "The Reflector: Learning from Runs",
      content: `The **Reflector** only analyzes "interesting" runs — not every one:

**Failures that required fixing** (iterations > 0):
- Extracts anti-pattern lessons from steps scoring below 0.5
- Categories: step_construction, selector_choice, error_handling, tool_usage
- Severity based on how low the score was (< 0.3 = critical)

**Strong first-attempt successes** (score > 0.8, iterations = 0):
- Extracts positive lessons from steps scoring above 0.85
- These become "DO" lessons (vs. "DON'T" from failures)

Unremarkable runs (moderate success, no fixes) are skipped.`,
      estimatedDuration: 1,
      details: `**How lessons are categorized:**

The weakest evaluation dimension determines the category:
- Specificity / Coverage → \`selector_choice\`
- Robustness → \`error_handling\`
- Executability → \`tool_usage\`
- All others → \`step_construction\`

Severity thresholds:
- min_score < 0.3 → Critical
- min_score < 0.5 → Important
- min_score >= 0.5 → Minor`,
    },
    {
      id: "curator",
      title: "The Curator: Managing the Playbook",
      content: `The **Curator** manages the playbook — a collection of actionable lessons injected into future generation prompts.

**Deduplication:** New lessons are compared against existing ones using word-overlap (Jaccard similarity > 70% = duplicate). Duplicates are merged, not added.

**Lifecycle:**
- \`staged\` → New entries start here
- \`active\` → Injected into generation prompts
- \`retired\` → Removed from prompts (underperforming)

**Retirement:** Entries applied 10+ times with < 20% helpfulness ratio are automatically retired. The playbook is capped at 200 entries.`,
      estimatedDuration: 1,
      tips: [
        "Lessons are grouped by severity when injected: Critical rules appear first",
        "Each lesson has a DO/DON'T prefix — e.g., \"DON'T use substring matching on error messages\"",
      ],
    },
    {
      id: "few-shot",
      title: "Few-Shot Curation Pipeline",
      content: `The **Few-Shot Curator** maintains a library of gold-standard verification step examples for injection into generation prompts.

**Quality gates for extraction:**
1. First-attempt success only (iterations = 0)
2. Evaluation score >= 0.8
3. Steps actually passed at runtime (execution-verified)

**Diversity:** Examples that are too similar to existing ones (by criterion description) are rejected. Max 20 examples per domain.

**Replacement:** When the library is full, a new example must score 0.1+ better than the weakest existing example to replace it.`,
      estimatedDuration: 1,
    },
    {
      id: "template-lifecycle",
      title: "Template Lifecycle Management",
      content: `Verification step templates go through a **lifecycle** based on runtime performance:

\`\`\`
Manual → Extracted → Promoted → (Demoted) → Retired
\`\`\`

**Transitions (require 10+ uses):**
- **Promotion:** Extracted templates with confidence >= 80% become Promoted
- **Demotion:** Promoted templates dropping to confidence <= 40% become Demoted
- **Retirement:** Templates with 10+ consecutive failures and zero successes are retired

Each transition is recorded as a **lifecycle event** with the confidence score at transition time.`,
      estimatedDuration: 1,
      details: `**Confidence calculation:**

\`confidence = success_count / (success_count + failure_count)\`

Templates also track:
- \`total_quality_score\` — sum of evaluation scores across all uses
- \`avg_quality\` — average quality score per use
- \`last_used_at\` — recency of template usage`,
    },
    {
      id: "gepa",
      title: "GEPA Prompt Evolution",
      content: `**GEPA** (Greedy Evolutionary Prompt Adaptation) uses DSPy to evolve generation prompts based on evaluation feedback.

**Architecture:** A Python sidecar service runs alongside the Rust runner:
- \`/gepa/optimize\` — general optimization
- \`/gepa/optimize/domain\` — per-domain optimization (Compilation, API, UI, etc.)

**How it works:**
1. Historical runs are sent as training examples
2. DSPy's \`BootstrapFewShotWithRandomSearch\` generates candidate programs
3. Each candidate is scored using the evaluation engine's ScoreWithFeedback
4. The best instruction set is returned for canary testing

**Trigger:** Every 20 workflow runs (with a 1-hour cooldown).`,
      estimatedDuration: 1,
      tips: [
        "GEPA optimization runs in a background thread via asyncio.to_thread to avoid blocking the server",
        "Only improvements exceeding 5% trigger canary testing — small gains are ignored",
      ],
      details: `**ScoreWithFeedback Metric:**

The metric calls the evaluation engine and formats structured feedback:
- Step scores below 0.6 → reports the weakest dimension with explanation
- Uncovered criteria → reports missing verification steps
- Quality gate failures → reports which gates failed

This feedback is consumed by DSPy's reflection LM to propose improved instructions.`,
    },
    {
      id: "prompt-injection",
      title: "How Learning Reaches the Generator",
      content: `All learned knowledge is injected into the **builder agent's prompt** during workflow generation.

The \`build_learning_context()\` function assembles two blocks:

**1. Playbook Section** — Active lessons grouped by severity:
\`\`\`
## Lessons from Previous Runs

### Critical (must follow):
- **DON'T**: Use substring matching on error messages
- **DO**: Add retry_count: 3 for HTTP checks

### Important:
- **DO**: Include timeout_seconds for all curl commands
\`\`\`

**2. Few-Shot Examples** — Gold-standard criterion → steps mappings:
\`\`\`
## Verified Examples

### Example 1 (quality: 0.92)
Criterion: API returns 200 for valid auth token
Steps: [{"check_type": "http", ...}]
\`\`\``,
      estimatedDuration: 1,
    },

    // ── Operational: Using the UI ────────────────────────────────────────
    {
      id: "navigate-dashboard",
      title: "Open the Adaptive Learning Dashboard",
      content: `Let's explore the three adaptive learning panels. Navigate to the **Meta-Optimizer** page and click the **Adaptive Learning** tab.

The dashboard shows:
- **Stat cards** — playbook size, curated examples, templates tracked, GEPA runs
- **Trend charts** — lesson growth, domain distribution, helpfulness over time
- **Template performance table** — success/failure rates and confidence scores`,
      action: 'Click the "Adaptive Learning" tab',
      targetElement: {
        selector: '[data-tutorial-id="learning-tab"]',
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      wait: {
        type: "dom-event",
        event: "click",
        selector: '[data-tutorial-id="learning-tab"]',
        timeout: 30000,
        onTimeout: "allow-skip",
        hint: 'Click the "Adaptive Learning" tab in the tab bar',
      },
      estimatedDuration: 1,
    },
    {
      id: "dashboard-overview",
      title: "Understanding the Dashboard",
      content: `The dashboard auto-refreshes every 60 seconds and whenever a workflow completes (via \`learning-update\` events).

**Stat Cards:**
- **Playbook Lessons** — total entries with active/staged breakdown
- **Curated Examples** — few-shot library size
- **Templates Tracked** — how many templates have performance data
- **GEPA Optimizations** — total prompt evolution runs

**Charts** (below the cards):
- **Lesson Growth** — bar chart of lessons added per day, colored by severity
- **Domain Distribution** — horizontal bars showing which domains generate the most lessons
- **Helpfulness Trend** — line chart showing how effective lessons are over time`,
      estimatedDuration: 1,
      tips: [
        "A rising helpfulness trend means the system is learning useful lessons",
        "If a domain dominates the distribution, it may need dedicated GEPA optimization",
      ],
    },
    {
      id: "navigate-playbook",
      title: "Browse the Playbook",
      content: `Click the **Playbook** tab to see individual lessons.

Lessons are grouped by severity: **Critical** (red), **Important** (yellow), **Minor** (gray).

Each lesson shows:
- **DO** / **DON'T** prefix indicating positive or negative guidance
- The lesson text
- Category label (e.g., "Step Construction", "Tool Usage")
- Helpfulness ratio (how often the lesson improved outcomes when applied)`,
      action: 'Click the "Playbook" tab',
      targetElement: {
        selector: '[data-tutorial-id="playbook-tab"]',
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      wait: {
        type: "dom-event",
        event: "click",
        selector: '[data-tutorial-id="playbook-tab"]',
        timeout: 30000,
        onTimeout: "allow-skip",
        hint: 'Click the "Playbook" tab',
      },
      estimatedDuration: 1,
    },
    {
      id: "playbook-controls",
      title: "Managing Playbook Entries",
      content: `Each lesson has interactive controls:

- **Activate** — move a staged lesson to active (starts injecting it into prompts)
- **Retire** — stop injecting an active lesson into prompts
- **Delete** — permanently remove a lesson (requires confirmation)

**Filtering:** Use the dropdowns to filter by domain (API, UI, Database, etc.) and status (Active, Staged, Retired).

**Detail view:** Click any lesson to expand it and see:
- Source run context (which workflow run produced this lesson)
- Run status, duration, iterations, and architecture`,
      estimatedDuration: 1,
      tips: [
        "Retiring a lesson doesn't delete it — you can re-activate it later",
        "Low helpfulness ratio (< 20%) means the lesson isn't helping and may be auto-retired",
      ],
    },
    {
      id: "navigate-evolution",
      title: "Prompt Evolution History",
      content: `Click the **Prompt Evolution** tab to see GEPA optimization runs.

This panel shows:
- **Domain summary cards** — click to filter by domain
- **Run history table** — each optimization run with before/after scores
- **Detail drill-down** — click a run to see the old and new prompts side-by-side`,
      action: 'Click the "Prompt Evolution" tab',
      targetElement: {
        selector: '[data-tutorial-id="prompt_evolution-tab"]',
        highlightType: "pulse",
        position: "bottom",
        allowInteraction: true,
      },
      wait: {
        type: "dom-event",
        event: "click",
        selector: '[data-tutorial-id="prompt_evolution-tab"]',
        timeout: 30000,
        onTimeout: "allow-skip",
        hint: 'Click the "Prompt Evolution" tab',
      },
      estimatedDuration: 1,
    },
    {
      id: "gepa-diff",
      title: "Reading Prompt Diffs",
      content: `When you expand a GEPA run, you see the **before** and **after** prompts:

- **Before** (red header) — the original generation instructions
- **After** (green header) — the optimized instructions GEPA proposed
- **Score delta** — how much the evaluation score improved

The improvement percentage tells you how much better the new prompt performed on the validation set. Only improvements > 5% are sent to canary testing.

**Domain cards** are clickable — click one to filter the table to that domain only.`,
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "The Full Picture",
      content: `**The adaptive learning cycle:**

\`\`\`
Workflow runs → Reflector extracts lessons → Curator manages playbook
                    ↓                              ↓
              Few-shot examples             Playbook lessons
                    ↓                              ↓
              Injected into ──────────── generation prompts
                                               ↓
                                    Better verification steps
                                               ↓
                                    Higher-quality workflows
                                               ↓
                                    (cycle repeats)
\`\`\`

**Periodic triggers:**
- Every 10 runs: template lifecycle transitions
- Every 20 runs: GEPA prompt evolution
- Every 100 runs: PRM model retraining

The system is self-improving — the more workflows you run, the smarter it gets.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "Plan 15: Adaptive Learning & Prompt Evolution",
          url: "file:///D:/qontinui-root/qontinui-dev-notes/session-prompts/15-adaptive-learning-prompt-evolution.md",
        },
      ],
    },
  ],
};
