/**
 * Online Learning Tutorial (Informational)
 *
 * Explains the continual online learning system that operates after every
 * workflow run: contextual bandit model routing, performance drift detection,
 * ADCA step credit attribution, post-run experience reflection, and the
 * evolvable strategy bank. Walks through the dashboard UI that visualizes
 * all of these subsystems.
 */

import type { Tutorial } from "../../../types/tutorial";

export const onlineLearningTutorial: Tutorial = {
  id: "online-learning",
  title: "Online Learning System",
  description:
    "Understand how Qontinui continuously adapts after every workflow run — using bandit-based model routing, drift detection, step credit attribution, experience accumulation, and strategy evolution.",
  duration: "10 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "meta-optimizer",
  category: "Architecture",
  tags: [
    "online-learning",
    "bandit",
    "drift-detection",
    "credit-assignment",
    "strategy",
    "featured",
  ],
  prerequisites: ["adaptive-learning"],
  learningObjectives: [
    "Understand the difference between batch meta-optimization and online learning",
    "Know how the UCB1 contextual bandit selects models per task context",
    "Understand ADWIN, DDM, and Page-Hinkley drift detectors and when each fires",
    "Know how ADCA credit assignment traces causal step contributions",
    "See how experiences are extracted and accumulated after each run",
    "Understand how strategies are extracted from P90+ runs and evolved via mutation",
    "Read the Online Learning dashboard to monitor all subsystems",
  ],
  steps: [
    // ── Overview ──────────────────────────────────────────────────────────
    {
      id: "intro",
      title: "Batch vs. Online Learning",
      content: `Qontinui has **two learning systems** that complement each other:

**Batch meta-optimizer** (existing):
- Triggers after N completed runs
- Analyzes historical data in bulk
- Produces recommendations reviewed by humans
- Canary A/B tests changes before applying

**Online learning** (new):
- Runs after **every single workflow completion**
- Updates models and detectors incrementally
- No human review needed — closed-loop
- Adapts in real-time to performance shifts

The online system **augments** batch optimization — it doesn't replace it. Batch handles big changes (prompt rewrites, architecture shifts). Online handles continuous tuning (model selection, drift response).`,
      estimatedDuration: 1,
      tips: [
        "Online learning runs as a non-blocking tokio::spawn — it never delays workflow execution",
        "All state is persisted to PostgreSQL and survives restarts",
      ],
    },
    {
      id: "pipeline",
      title: "The Post-Run Pipeline",
      content: `After every workflow completes (success or failure), a 5-stage pipeline runs:

1. **Enrich from traces** — loads pipeline agent traces from PG for real duration, cost, and downstream success data
2. **Credit assignment** — computes per-step causal contribution using 5 attribution signals
3. **Model routing update** — feeds the outcome reward into the contextual bandit Q-table
4. **Drift detection** — updates ADWIN, DDM, and Page-Hinkley detectors per metric per context
5. **Experience reflection** — extracts structured lessons and summaries

Then everything is **persisted to 8 PostgreSQL tables**: model_routing_table, model_routing_decisions, performance_drift_signals, drift_detector_state, step_credit_assignments, experience_summaries, strategy_bank, and learning_outcomes.model_used.`,
      estimatedDuration: 1,
      details: `The pipeline is triggered from \`task_lifecycle.rs\` in both \`mark_task_completed\` and \`mark_task_failed\`. Internal runs (meta-optimizer, fixer, reflection) are skipped to prevent recursion. A 3-second delay allows deterministic metrics to be recorded first.`,
    },
    // ── Model Routing ─────────────────────────────────────────────────────
    {
      id: "bandit-overview",
      title: "Contextual Bandit Model Routing",
      content: `The static AI router uses keyword heuristics to pick models:
- Short prompt + few files = Haiku
- Complex keywords + many files = Opus
- Everything else = Sonnet

The **online bandit** replaces this with data-driven selection. It's a **UCB1 contextual bandit**:

- **Context**: prompt length bucket, file count bucket, complexity tier, domain, has_ui (450 states)
- **Arms**: available models (Haiku, Sonnet, Opus)
- **Reward**: composite_agentic_score - 0.01 * cost_usd (quality minus cost)
- **Policy**: UCB1 — naturally explores under-tested arms via confidence bounds

When a new model is released, UCB1 automatically explores it because unvisited arms get infinite upper confidence bounds.`,
      estimatedDuration: 1,
      tips: [
        "The bandit activates only after 3+ visits per context — before that, it falls back to the static router",
        "Manual overrides can lock specific contexts to specific models",
      ],
    },
    {
      id: "bandit-dashboard",
      title: "Model Routing on the Dashboard",
      content: `The **Model Routing** tab shows:

- **Model badges** at the top — which models are available as arms
- **Q-table** — every (context, model) pair with its learned Q-value and visit count
- Sorted by Q-value descending — the best-performing model per context appears first
- **Q-values between 0 and 1** — closer to 1 means higher quality at lower cost

The **Overview** tab also shows a **Policy Preview**: the recommended model per context (the greedy arm), helping you quickly see which model wins for which task type.`,
      targetElement: {
        selector: '[data-tutorial-id="online-learning-tabs"]',
        highlightType: "border",
        position: "bottom",
      },
      estimatedDuration: 1,
    },
    // ── Drift Detection ───────────────────────────────────────────────────
    {
      id: "drift-overview",
      title: "Performance Drift Detection",
      content: `The system monitors 4 metrics **per context** for distributional shifts:

| Detector | Metric | Signal |
|----------|--------|--------|
| **ADWIN** | composite_score, cost, duration | Adaptive window detects mean shifts |
| **DDM** | success_rate (binary) | Error rate exceeds 3 sigma from minimum |
| **Page-Hinkley** | cost, duration | Gradual trend changes |

Each detector runs independently per \`(metric, context_key)\`. Drift in "backend:complex:no_ui" tasks doesn't affect "frontend:trivial:ui" detection.

When drift is detected:
1. A **signal is persisted** to \`performance_drift_signals\`
2. A **Tauri event** is emitted for real-time UI notification
3. The model router **boosts exploration** for the affected context (halves visit counts)`,
      estimatedDuration: 1,
      details: `ADWIN (Bifet & Gavalda 2007) maintains a compressed exponential histogram of observations. When a statistically significant difference is found between two sub-windows, the older data is discarded. DDM (Gama et al. 2004) tracks running error rate and standard deviation — warning at 2 sigma, drift at 3 sigma. Page-Hinkley tracks cumulative deviations from the running mean.`,
    },
    {
      id: "drift-dashboard",
      title: "Drift Signals on the Dashboard",
      content: `The **Drift Detection** tab shows:

- **Monitor status** — number of active detector instances and algorithm names
- **Signals table** — each row shows: severity badge (red = drift, amber = warning), metric name, context key, detector type, and the pre/post drift mean values

The **Overview** tab shows an **amber alert banner** when any unacknowledged drift signals exist, with up to 5 recent signals previewed.

The drift badge count on the **Drift Detection tab** updates in real-time when the backend emits a \`drift-detected\` event.`,
      estimatedDuration: 1,
    },
    // ── Credit Assignment ─────────────────────────────────────────────────
    {
      id: "credit-overview",
      title: "ADCA Step Credit Attribution",
      content: `Instead of scoring steps independently, **Attribution-Driven Credit Assignment (ADCA)** traces the causal contribution of each step to the final outcome.

**5 attribution signals** per step:

1. **Temporal proximity** — steps closer to the outcome get exponentially more credit
2. **Output utilization** — steps whose output was consumed downstream (from handoff context)
3. **Confidence delta** — steps that improved verification confidence
4. **Downstream success** — steps followed by successful continuations
5. **Cost efficiency** — penalizes expensive steps with low impact

Credits are **normalized** so they sum to the workflow's composite score. This means a step's credit directly represents its fraction of the outcome quality.`,
      estimatedDuration: 1,
      tips: [
        "Credit data comes from pipeline_agent_traces — runs without multi-agent pipelines won't have step attributions",
        "The step-type scorecard aggregates credits across all runs to identify consistently high/low contributors",
      ],
    },
    {
      id: "credit-dashboard",
      title: "Step Credits on the Dashboard",
      content: `The **Step Credits** tab shows the **step-type scorecard**:

- Each row is a step type (e.g., "spec_analyst", "implementer", "verifier")
- **Avg Credit** — mean normalized credit across all runs (blue, monospace)
- **Raw Credit** — mean un-normalized credit (multiplicative signal combination)
- **Count** — how many times this step type has been observed
- **Bar** — green horizontal bar proportional to the credit magnitude

Higher credit = that step type consistently contributes more to positive outcomes. Step types with near-zero credit may be candidates for optimization or removal.`,
      estimatedDuration: 1,
    },
    // ── Experience Reflection ─────────────────────────────────────────────
    {
      id: "reflection-overview",
      title: "Post-Run Experience Reflection",
      content: `After each run, the system extracts structured experience data:

**Step-level lessons** (deterministic, no LLM):
- Repeated failures (>3 retries) become anti-pattern knowledge
- Timeout errors are flagged with step type and duration
- Excessive tool usage (>10 calls before failure) suggests unclear instructions

**Run-level experience summary**:
- Key decisions that shaped the outcome
- Failure points (with recoverability analysis)
- Effective patterns (e.g., "zero-retry successful workflow")

These are persisted to \`experience_summaries\` and can be queried by domain to understand what works in different contexts.`,
      estimatedDuration: 1,
    },
    // ── Strategy Bank ─────────────────────────────────────────────────────
    {
      id: "strategy-overview",
      title: "Evolvable Strategy Bank",
      content: `A **strategy** is a higher-level pattern encoding *how to approach* a class of tasks. It composes: preferred architecture, preferred model, step-type overrides, and parameter preferences.

**Three creation mechanisms:**

1. **Extraction** — When a run achieves composite_score > P90 for its context, its configuration is extracted as a **Candidate** strategy
2. **Failure mining** — When the same context has both failing and succeeding runs with different configs, the successful config becomes a strategy
3. **Mutation** — Top strategies get variants with small modifications (swap model, change architecture)

**Lifecycle:** Candidate \u2192 Active (after canary validation) \u2192 Degraded (if drift detected) \u2192 Retired

Strategies are selected via **Thompson Sampling** — a Bayesian policy that naturally accounts for uncertainty in performance estimates.`,
      estimatedDuration: 1,
      tips: [
        "Candidate strategies must pass the existing canary rollout system (p < 0.05) before becoming Active",
        "The P90 threshold is computed per-context, not globally — a 0.7 score may be P90 for complex tasks",
      ],
    },
    {
      id: "strategy-dashboard",
      title: "Strategies on the Dashboard",
      content: `The **Strategies** tab shows cards for each strategy:

- **Name** — describes the origin (e.g., "Extracted: backend complex (score 95%)")
- **Status badge** — green for Active, blue for Candidate, grey for Degraded/Retired
- **Description** — how the strategy was created and what it configures
- **Performance stats** — times selected, success rate, average composite score, average cost

The **Experiences** tab shows recent run summaries with outcome badges, domain/complexity context, and counts of key decisions, failure points, and effective patterns.`,
      estimatedDuration: 1,
    },
    // ── Summary ───────────────────────────────────────────────────────────
    {
      id: "summary",
      title: "System Overview",
      content: `The online learning system closes the loop between execution and optimization:

**Every run** \u2192 Credit assignment + Routing update + Drift check + Experience extraction + Strategy evaluation

**8 PostgreSQL tables** persist all state across restarts.

**8 MCP API endpoints** under \`/online-learning/*\` serve the dashboard.

**3 routing integration points** in \`ai_provider/routing.rs\` use the bandit for live model selection.

The system starts learning from the very first workflow run. After 3+ runs per context, the bandit activates. After 20+ runs, drift detectors have enough baseline data to detect shifts. After 30+ runs, P90 strategy extraction becomes meaningful.

To monitor: open the **Online Learning** tab in the sidebar under DEV.`,
      estimatedDuration: 1,
      resources: [
        {
          title: "ADWIN Paper (Bifet & Gavalda 2007)",
          url: "https://link.springer.com/chapter/10.1007/978-3-540-75488-6_5",
          type: "article",
        },
        {
          title: "UCB1 Algorithm",
          url: "https://en.wikipedia.org/wiki/Upper_confidence_bound",
          type: "article",
        },
      ],
    },
  ],
};
