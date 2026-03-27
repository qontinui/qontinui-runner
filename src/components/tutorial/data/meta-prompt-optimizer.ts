/**
 * Meta-Prompt Optimizer Tutorial (Informational)
 *
 * Explains the 4th optimizer agent that uses an LLM to iteratively rewrite
 * prompts based on scored evaluation results, producing concrete prompt
 * variants for canary A/B testing. Covers the extraction pipeline, the
 * Generate-Critique-Refine loop, canary integration, the feedback loop
 * with similarity guard and circuit breaker, and the frontend dashboard.
 */

import type { Tutorial } from "../../../types/tutorial";

export const metaPromptOptimizerTutorial: Tutorial = {
  id: "meta-prompt-optimizer",
  title: "Meta-Prompt Optimizer",
  description:
    "Understand the LLM-based prompt rewriting system that analyzes prompt performance, generates improved variants, and validates them through canary A/B testing with a multi-layered feedback loop.",
  duration: "15 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "meta-optimizer",
  category: "Architecture",
  tags: [
    "meta-optimizer",
    "prompt-engineering",
    "canary-testing",
    "a-b-testing",
    "feedback-loop",
    "featured",
  ],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand why prompt optimization matters and what problem it solves",
    "Know how prompt samples are extracted and grouped by performance",
    "Understand the LLM-based Generate-Critique-Refine rewriting loop",
    "See how rewrites are validated through robustness checks and similarity guards",
    "Understand the canary A/B testing pipeline that validates prompt changes",
    "Learn how the feedback loop prevents repeating failed approaches",
    "Know the circuit breaker and adaptive cooldown system",
    "Use the Prompt Optimization tab to monitor and trigger optimizations",
  ],
  steps: [
    {
      id: "intro",
      title: "The Prompt Stagnation Problem",
      content: `The meta-optimizer system has three existing optimizers that tune **parameters, rules, and agent config**. But none of them touch the actual **prompt text** sent to AI agents.

**The problem:** When a prompt systematically leads to poor outcomes (e.g., the verifier agent produces unstructured responses because the prompt doesn't specify JSON format), no optimizer adjusts it. Recommendations sit in PENDING status indefinitely because they're abstract parameter changes, not testable artifacts.

**The solution:** The **Meta-Prompt Optimizer** (Agent 4) uses an LLM to analyze prompt performance data, critique underperforming prompts, rewrite them, and validate the rewrites through canary A/B testing. Every change produces a concrete, testable prompt variant.`,
      estimatedDuration: 1,
      tips: [
        "This is the 4th optimizer alongside Pipeline Prompt, Architecture, and Generation Template optimizers",
        "It never auto-applies changes — every rewrite goes through canary testing first",
      ],
    },
    {
      id: "extraction",
      title: "Step 1: Prompt Sample Extraction",
      content: `The optimizer starts by collecting performance data from recent workflow runs.

**Data Sources:**
- \`phase_token_usage\` — records which phase (generation, verification, agentic) was executed per run
- \`learning_outcomes\` — stores the \`composite_agentic_score\` (0.0–1.0) for each run
- \`prompt_registry\` — stores active prompt variants per agent type

**Grouping:** Samples are grouped by **(phase, agent_type)** to produce per-group metrics:

| Metric | What it measures |
|--------|-----------------|
| **mean_score** | Average composite score (0.0–1.0) |
| **failure_rate** | Percentage of runs with status "failed" |
| **mean_iterations** | Average iterations to completion |
| **mean_cost** | Average cost in USD per run |

The worst-performing group (highest failure rate, >= 10 samples, > 30% failure rate) becomes the optimization target.`,
      estimatedDuration: 1,
      details: `**Default Prompt Resolution:**

When no variant is active in the registry, the system falls back to hardcoded default prompts for each agent type:

- **locator** — JSON output format for file mapping with criteria, confidence, target files
- **verifier** — Structured assessment with status (pass/partial/fail/unreachable), confidence, observations
- **implementer** — Criteria-driven code change instructions with locator context
- **agentic_fixer** — Targeted fix instructions based on verification failure output

This ensures the optimizer can always see and critique the actual prompt being used, even before any variants have been created.`,
    },
    {
      id: "rewriter",
      title: "Step 2: LLM-Based Prompt Rewriting",
      content: `The core of the optimizer is a **Generate-Critique-Refine** loop run by an LLM. The optimizer is itself a LoopConfig workflow (same as the other 3 optimizers) that runs asynchronously.

**Setup Steps** load context via HTTP:
1. Optimizer history and baselines
2. Prompt group metrics (from extraction)
3. Active prompt variants
4. **Evolution history** (previous rewrite attempts and their verdicts)
5. Recent workflow outcomes (L1)
6. Agent trace summaries (L0)

**Thinking Styles** rotate across runs to prevent convergence on a single approach:

| Style | Focus |
|-------|-------|
| **Evidence-driven** | Ground changes in specific failure examples |
| **Structural clarity** | Restructure for clear step-by-step flow |
| **Constraint-tightening** | Add explicit guardrails and output format specs |
| **Few-shot exemplar** | Add concrete input/output examples |`,
      estimatedDuration: 2,
      tips: [
        "The optimizer produces [META_PROMPT_REWRITE] markers in its output, just like the other optimizers produce their recommendation markers",
        "Maximum 1 rewrite per run — it focuses on the single worst prompt",
      ],
    },
    {
      id: "validation",
      title: "Step 3: Multi-Layer Validation",
      content: `Before a rewritten prompt enters canary testing, it passes through **three validation gates**:

**Gate 1: Robustness Check** (\`validate_prompt_robustness\`)
Checks for: role definition, output format spec, boundary constraints, minimum length. Prompts with > 2 warnings are rejected.

**Gate 2: Semantic Similarity Guard** (\`trigram_similarity\`)
Computes text similarity against all previously **rejected** prompts for this agent type. If similarity > 90%, the rewrite is blocked — it's too close to something that already failed.

**Gate 3: Content Deduplication** (\`compute_content_hash\`)
SHA256 hash comparison prevents creating duplicate recommendations. If an identical recommendation already exists in pending/canary/applied status, it's skipped.

Only prompts that pass all three gates proceed to variant creation and canary testing.`,
      estimatedDuration: 1,
      details: `**Why the similarity guard matters:**

Without it, the optimizer can oscillate between similar failed approaches:
1. Run 1: "Add JSON output format" → rejected (score dropped)
2. Run 2: "Add structured JSON output" → nearly identical, would also fail
3. Run 3: "Require JSON response format" → again nearly identical

The trigram similarity gate (Dice coefficient on 3-character substrings) catches this and forces the optimizer to try a fundamentally different approach, like adding few-shot examples instead.`,
    },
    {
      id: "canary",
      title: "Step 4: Canary A/B Testing",
      content: `Every validated rewrite creates three linked records:

1. **Prompt Variant** — the new prompt text, stored in \`prompt_registry\` (initially inactive)
2. **Recommendation** — links the variant to the optimizer run with evidence
3. **Canary Rollout** — starts A/B testing at 20% traffic

**How canary works:**
- 20% of future runs for this agent use the new prompt variant
- 80% continue using the current prompt (baseline)
- After **10+ canary runs**, statistical evaluation begins

**Statistical verdict** (shared engine with all optimizers):
- **Promote** — canary success rate significantly better (z-test, p < 0.05)
- **Rollback** — canary success rate significantly worse
- **Continue** — insufficient evidence, extend evaluation

For borderline results with 30+ runs, the system automatically **increases canary traffic** (+10%, up to 50%) to gather data faster.`,
      estimatedDuration: 2,
      tips: [
        "Canary evaluation uses Cohen's h effect size and 95% confidence intervals",
        "Stale canaries (active > 30 days without verdict) are automatically rolled back",
      ],
    },
    {
      id: "feedback-loop",
      title: "Step 5: The Feedback Loop",
      content: `The \`prompt_evolution\` table tracks every rewrite attempt and its outcome, creating a **legible history** of prompt optimization:

| Column | Purpose |
|--------|---------|
| \`parent_variant_id\` | Links to the previous version (evolution chain) |
| \`canary_verdict\` | adopt, reject, or NULL (pending) |
| \`score_before\` / \`score_after\` | Quantifies the impact |
| \`baseline_prompt_hash\` | Detects when the baseline prompt changed in code |
| \`consecutive_rejections\` | Powers the circuit breaker |

**After canary completes:**
- **Adopt** → activate the new variant, mark recommendation APPLIED
- **Reject** → mark INEFFECTIVE, record in evolution history for the next round
- **Inconclusive** → extend canary traffic percentage

The next optimizer run **reads the evolution history** and sees rejected approaches. The LLM prompt explicitly instructs: "Previous attempt tried X, which was rejected. Take a different approach."`,
      estimatedDuration: 2,
      details: `**Baseline Drift Detection:**

The \`baseline_prompt_hash\` column stores a SHA256 hash of the baseline prompt at rewrite time. If someone edits the hardcoded prompt in code while a canary is running, the system detects the drift and warns that canary results may be invalid — you'd be comparing against a prompt that no longer exists.`,
    },
    {
      id: "circuit-breaker",
      title: "The Circuit Breaker",
      content: `What happens when multiple rewrites in a row fail? The system has a **diminishing returns circuit breaker** that prevents wasting tokens on a prompt that may already be near-optimal.

**Adaptive Cooldown** (exponential backoff per agent_type):

| Consecutive Rejections | Cooldown Period |
|----------------------|-----------------|
| 0 | 24 hours (standard) |
| 1 | 3 days |
| 2 | 1 week |
| 3 | 2 weeks |
| 4+ | 4 weeks |

**LLM Awareness:** The circuit breaker status is injected into the optimizer's prompt. After 3+ rejections, the LLM is explicitly told:
- The obvious fixes have all failed
- Consider structural alternatives (wrong agent responsibilities, missing context)
- Or accept that the prompt may already be near-optimal and output ZERO rewrites

This prevents the optimizer from endlessly generating variations of the same failed approach.`,
      estimatedDuration: 1,
      tips: [
        "The cooldown is per agent_type — if 'verifier' is in cooldown, 'implementer' can still be optimized",
        "Manual triggers from the UI still check the active-canary guard but bypass the cooldown",
      ],
    },
    {
      id: "trigger-guards",
      title: "When Does the Optimizer Run?",
      content: `The meta-prompt optimizer runs automatically as part of the periodic trigger cycle (same as the other 3 optimizers). But it has **additional guards**:

**Standard guards** (shared with all optimizers):
1. No optimizer of any type already running
2. Meta-optimizer enabled in settings
3. Threshold met (N completed runs since last optimizer)
4. Source run is not a meta-optimizer/fixer/reflection

**Meta-prompt specific guards:**
5. A prompt group exists with **failure rate > 30%** and **>= 10 samples**
6. **No active canary** for the target agent type
7. **Adaptive cooldown** not exceeded (24h to 4 weeks based on rejection history)
8. **Baseline drift warning** (non-blocking) when hardcoded prompts changed

All guards must pass for the optimizer to launch. Manual triggers from the UI check guards 5-6 but skip the cooldown.`,
      estimatedDuration: 1,
    },
    {
      id: "architecture",
      title: "Architecture Overview",
      content: `The meta-prompt optimizer is built from **6 Rust modules** and **1 React component**:

**Rust Backend:**
- \`prompt_extractor.rs\` — Sample extraction, grouping, metrics, default prompt resolver
- \`meta_prompt_optimizer.rs\` — LoopConfig, setup steps, agentic prompt template
- \`prompt_evolution.rs\` — Evolution CRUD, cooldown, similarity, circuit breaker
- \`parser.rs\` — [META_PROMPT_REWRITE] marker parsing + validation pipeline
- \`trigger.rs\` — Meta-prompt specific guards integrated into optimizer dispatch
- \`canary.rs\` — Verdict → evolution feedback wiring

**Frontend:**
- \`PromptOptimizationTab.tsx\` — Dashboard with groups, canaries, evolution history, diff view

**Database:**
- \`prompt_evolution\` table (SQLite + PG dual-write)
- \`prompt_registry\` table (existing, stores variants)
- \`canary_rollouts\` table (existing, manages A/B tests)

**HTTP API:**
4 endpoints under \`/meta-optimizer/prompt-optimization/\` for status, metrics, evidence, and evolution history.`,
      estimatedDuration: 1,
      details: `**Data Flow:**

\`\`\`
phase_token_usage + learning_outcomes
        ↓ (extract_prompt_samples)
  PromptGroupMetrics (grouped by phase/agent_type)
        ↓ (select worst group > 30% failure rate)
  LLM Generate-Critique-Refine loop
        ↓ (parse [META_PROMPT_REWRITE] markers)
  Robustness → Similarity → Dedup validation
        ↓ (create_variant + create_recommendation)
  PromptVariant + CanaryRollout (20% traffic)
        ↓ (auto_evaluate_canaries after 10+ runs)
  Promote / Rollback / Extend
        ↓ (update_evolution_for_recommendation)
  prompt_evolution history → fed back into next run
\`\`\``,
    },
    {
      id: "dashboard",
      title: "Using the Prompt Optimization Tab",
      content: `The **Prompt Optimization** tab on the Meta-Optimizer page gives you visibility into the system:

**Prompt Groups by Failure Rate** — shows all prompt groups ranked by failure rate (worst first). Each group shows:
- Phase/agent type
- Failure rate (red if > 30%)
- Mean score
- Sample count
- Expandable prompt text (from active variant or hardcoded default)

**Active Canaries** — yellow banner showing in-progress A/B tests with baseline score

**Evolution History** — timeline of all rewrite attempts with:
- Score before → score after (green = improved, red = regressed)
- Verdict badge (pending, adopt, reject)
- Consecutive rejection count
- Expandable critique and changes summary
- **View Diff** button — shows line-level diff between old and new prompt

**Trigger Optimization** button — manually launch the optimizer (checks active-canary guard).`,
      estimatedDuration: 1,
      targetElement: {
        selector: '[data-tutorial-id="prompt-optimization-tab"]',
        highlightType: "border",
        position: "bottom",
      },
    },
    {
      id: "mental-model",
      title: "Mental Model Summary",
      content: `Think of the meta-prompt optimizer as an **automated prompt engineer** with guardrails:

1. **Observe** — extract prompt performance data and find the worst-performing prompt
2. **Hypothesize** — use an LLM to critique the prompt and generate an improved version
3. **Validate** — check robustness, similarity to past failures, and deduplication
4. **Experiment** — run an A/B test (canary) to compare new vs. old
5. **Learn** — record the outcome and feed it back into the next iteration
6. **Backoff** — if improvements keep failing, exponentially reduce attempt frequency

This is a **closed-loop optimization system** — not a one-shot rewriter. Each iteration builds on the knowledge from previous attempts, and the system self-regulates to avoid wasting resources on near-optimal prompts.`,
      estimatedDuration: 1,
      tips: [
        "The system is designed to eventually converge: either it finds a better prompt, or the circuit breaker stops it",
        "All data is in SQLite (source of truth) with PG dual-write for analytics",
      ],
      resources: [
        {
          title: "Session Prompt: 04-meta-prompt-optimizer.md",
          url: "qontinui-dev-notes/session-prompts/04-meta-prompt-optimizer.md",
        },
      ],
    },
  ],
};
