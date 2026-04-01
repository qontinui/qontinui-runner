/**
 * Opik Integration Tutorial
 *
 * Informational tutorial explaining the Opik-inspired LLM observability,
 * evaluation, prompt versioning, feedback scoring, and cost tracking system.
 */

import type { Tutorial } from "../../../types/tutorial";

export const opikIntegrationTutorial: Tutorial = {
  id: "opik-integration",
  title: "LLM Observability & Evaluation",
  description:
    "Understanding the Opik-inspired LLM observability, evaluation, and prompt management system integrated into Qontinui.",
  duration: "15 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "llm-analytics",
  category: "AI Features",
  tags: ["opik", "llm", "observability", "evaluation", "featured"],
  prerequisites: ["ai-analysis"],
  learningObjectives: [
    "Understand how LLM calls are traced and their costs tracked",
    "Learn how the evaluation framework tests prompt quality",
    "Understand prompt versioning and A/B testing with canaries",
    "See how feedback scores flow from automated metrics to the dashboard",
    "Understand the dual SQLite/PostgreSQL storage architecture",
  ],
  steps: [
    {
      id: "welcome",
      title: "LLM Observability & Evaluation",
      content: `Qontinui includes a full **Opik-inspired observability and evaluation system** for every LLM call made during workflow execution.

**The 5 Pillars:**
- **Tracing** — every LLM call is captured with model, tokens, cost, and latency
- **Evaluation** — dataset-driven experiments test prompt quality with pluggable metrics
- **Versioning** — every prompt change creates a new immutable version with diff support
- **Feedback** — quality scores flow from automated metrics, LLM judges, and manual review
- **Cost Tracking** — per-run, per-model, and per-action cost breakdowns with trend analysis

This tutorial walks through each pillar and how the pieces connect.`,
      details: `The system spans three repositories: the Tauri runner (Rust backend + React frontend), the web backend (Python/FastAPI), and the SDK packages. The runner captures LLM telemetry in Rust phase executors, stores it locally in SQLite, and pushes it to the PostgreSQL-backed web API for dashboard consumption. The frontend uses both Tauri invoke commands and REST fetch calls depending on the data source.`,
      tips: [
        "This is an informational tour — no actions required, just read and explore",
        "Expand the 'Details' sections for architecture deep-dives and file paths",
      ],
      estimatedDuration: 1,
    },
    {
      id: "llm-call-tracing",
      title: "LLM Call Tracing",
      content: `Every AI action in a workflow execution is **automatically traced**. When a phase executor calls an LLM, the system captures:

- **Model & Provider** — which model handled the request (e.g., claude-sonnet-4-20250514 via Anthropic)
- **Token Counts** — input tokens, output tokens, and cache read/write tokens
- **Cost** — calculated from per-model pricing tables
- **Latency** — wall-clock time from request to response
- **Parent Linkage** — each trace links to its ExecutionRun and ActionExecution

Traces form a **hierarchical span tree**: an ExecutionRun contains ActionExecutions, each of which may contain multiple LLM calls.`,
      details: `Phase executors call \`build_llm_metrics()\` after each LLM response, populating an \`LlmCallMetrics\` struct. This flows through the \`ExecutionReportingService\` which persists it to SQLite via \`llm_metrics_repo\` and optionally pushes to the web backend. The span hierarchy uses \`execution_run_id\` and \`action_execution_id\` foreign keys to maintain parent-child relationships.`,
      tips: [
        "Tracing is zero-config — it happens automatically for every LLM call in every workflow",
      ],
      targetElement: {
        selector: '[data-tutorial-id="llm-observability-dashboard"]',
        highlightType: "border",
        position: "bottom",
      },
      estimatedDuration: 1,
    },
    {
      id: "cost-analytics",
      title: "Cost Analytics Summary",
      content: `The **summary cards** give you an at-a-glance view of LLM spending:

- **Total Cost** — aggregate dollar amount across all traced calls
- **Total Tokens** — combined input + output token count
- **Total Calls** — number of LLM invocations
- **Avg Cost/Call** — helps identify expensive prompts

These metrics update based on the selected time range and can be filtered by model or provider.`,
      details: `Data comes from two sources depending on availability. The Tauri \`invoke\` command queries SQLite directly for offline/local data. The \`fetch\` API calls the web backend's \`/llm-analytics/summary\` endpoint which queries PostgreSQL. The frontend merges both sources, preferring PG data when available. The response follows the \`LLMAnalyticsSummary\` schema with fields for total_cost, total_tokens, total_calls, and avg_cost_per_call.`,
      tips: [
        "Check summary cards after major workflow runs to catch unexpectedly expensive prompts",
        "Use the time range filter to compare costs across different periods",
      ],
      targetElement: {
        selector: '[data-tutorial-id="llm-summary-cards"]',
        highlightType: "border",
        position: "bottom",
      },
      estimatedDuration: 1,
    },
    {
      id: "cost-trends",
      title: "Cost Trend Analysis",
      content: `The **cost trend chart** visualizes daily LLM spending with a **7-day moving average** trend line.

- The bar chart shows raw daily cost
- The overlay line shows the smoothed trend, making it easy to spot cost increases
- Hover over any data point to see the exact cost and call count for that day

This helps you catch cost regressions early — for example, if a prompt change doubles token usage.`,
      details: `The chart data comes from the \`CostTrendResponse\` schema, which includes an array of \`{ date, cost, calls }\` objects. The frontend \`useCostTrends\` hook fetches this from \`/llm-analytics/cost-trends\` with start/end date parameters. The 7-day moving average is calculated client-side for responsive filtering.`,
      tips: ["A sudden spike in the trend line often indicates a prompt regression or retry loop"],
      targetElement: {
        selector: '[data-tutorial-id="llm-cost-trend-chart"]',
        highlightType: "border",
        position: "top",
      },
      estimatedDuration: 1,
    },
    {
      id: "evaluation-framework",
      title: "Evaluation Framework Overview",
      content: `The evaluation system lets you **systematically test prompt quality** using the formula:

**Datasets + Experiments + Metrics = Evaluation**

- **Datasets** — versioned collections of input/expected-output pairs
- **Experiments** — runs of a prompt variant against a dataset
- **Metrics** — scoring functions (LLM-as-judge, exact match, similarity)
- **Results** — per-item scores with statistical aggregation

This is inspired by Opik's dataset-driven evaluation approach, adapted for Qontinui's workflow context.`,
      details: `The backend uses 4 SQLAlchemy models: \`EvaluationDataset\`, \`DatasetItem\`, \`EvaluationExperiment\`, and \`ExperimentResult\`. Content hashing (SHA256) provides deduplication — identical dataset items are not duplicated across versions. Version numbers auto-increment per dataset. The API supports CRUD operations plus bulk import/export.`,
      tips: [
        "Start with a small dataset (10-20 items) to iterate quickly on prompt changes",
        "Evaluation is the foundation for confident prompt updates — always test before deploying",
      ],
      targetElement: {
        selector: '[data-tutorial-id="evaluation-dashboard"]',
        highlightType: "border",
        position: "bottom",
      },
      estimatedDuration: 1,
    },
    {
      id: "datasets",
      title: "Evaluation Datasets",
      content: `Datasets are **versioned collections** of test cases, each containing:

- **Input** — the prompt input or context to send to the LLM
- **Expected Output** — the reference answer or expected behavior
- **Metadata** — optional tags, categories, or difficulty labels

You can **import and export** datasets as JSON or CSV, making it easy to share test suites across environments or with team members.`,
      details: `Each dataset item is content-hashed using SHA256 over the concatenation of input + expected_output. This means importing the same item twice will not create a duplicate. The \`EvaluationDataset\` model tracks name, description, version, and item_count. The \`DatasetItem\` model holds input, expected_output, metadata (JSON), and the content_hash. Versions auto-increment when items are added or modified.`,
      tips: [
        "Use CSV import to bootstrap datasets from existing test cases or spreadsheets",
        "Tag items with difficulty levels to analyze where your prompts struggle most",
      ],
      targetElement: {
        selector: '[data-tutorial-id="evaluation-datasets-panel"]',
        highlightType: "border",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "llm-as-judge",
      title: "LLM-as-Judge Evaluation",
      content: `The system includes **4 built-in judge metrics** that use an LLM to evaluate response quality:

- **Hallucination** — detects fabricated facts not supported by the input context
- **Answer Relevance** — measures whether the response addresses the actual question
- **Factuality** — checks factual accuracy against provided reference material
- **Content Safety** — flags harmful, biased, or inappropriate content

Each metric constructs a specialized judge prompt, sends it to the LLM, and parses a structured response with a **score between 0 and 1**.`,
      details: `The judge metrics are implemented in Rust in \`llm_judge_metrics.rs\`. Each metric defines a \`JudgeInput\` struct (containing the original input, LLM output, and optional reference) and returns a \`JudgeResult\` with a score (f64 clamped to 0.0-1.0), explanation string, and metric name. The judge prompts use few-shot examples and chain-of-thought reasoning to produce consistent scores. Results are stored as \`ExperimentResult\` records linked to the experiment and dataset item.`,
      tips: [
        "Hallucination detection is especially valuable for RAG workflows where grounding matters",
        "Run all 4 judges together for a comprehensive quality profile of your prompts",
      ],
      targetElement: {
        selector: '[data-tutorial-id="live-evaluation-panel"]',
        highlightType: "border",
        position: "right",
      },
      estimatedDuration: 1,
    },
    {
      id: "experiment-results",
      title: "Experiment Results",
      content: `After running an experiment, the **results table** shows per-item performance:

- **Score** — the metric score for each dataset item (0-1 scale)
- **Cost** — how much the LLM call cost for that item
- **Latency** — response time in milliseconds
- **Token Counts** — input and output tokens used

You can **sort by any column** to find your worst-performing items, most expensive calls, or slowest responses. Use this to do **statistical comparison between prompt variants**.`,
      details: `The results table uses server-side pagination (20 items per page) with sortable columns. Each row corresponds to an \`ExperimentResult\` record linking an \`EvaluationExperiment\` to a \`DatasetItem\`. The table computes aggregate statistics (mean, median, std dev, min, max) across all results for the experiment. Comparing two experiments side-by-side shows which prompt variant performs better on each metric.`,
      tips: [
        "Sort by score ascending to find items where your prompt struggles most — these are your improvement targets",
      ],
      targetElement: {
        selector: '[data-tutorial-id="experiment-results-table"]',
        highlightType: "border",
        position: "top",
      },
      estimatedDuration: 1,
    },
    {
      id: "feedback-scores",
      title: "Feedback Scores",
      content: `**Feedback scores** are quality ratings attached to individual runs and actions. They come from three sources:

- **Automated** — the runner's agentic metrics (task_completion, step_efficiency, etc.) are automatically converted to feedback scores after every run
- **LLM Judge** — evaluation experiments produce scores via the judge metrics
- **Manual** — you can add human ratings directly from the dashboard

Scores range from 0 to 1 and include a category label and optional explanation.`,
      details: `There are 11 agentic metric types that auto-push as feedback: task_completion, step_efficiency, error_recovery, goal_alignment, context_utilization, decision_quality, resource_efficiency, adaptation_score, output_quality, time_efficiency, and overall_performance. These are computed in Rust after each execution run completes and batch-posted to the web backend's \`/feedback-scores/batch\` endpoint.`,
      tips: [
        "Manual feedback scores are valuable for capturing quality nuances that automated metrics miss",
        "Use feedback trends over time to track whether your prompts are improving or regressing",
      ],
      targetElement: {
        selector: '[data-tutorial-id="feedback-scores-panel"]',
        highlightType: "border",
        position: "left",
      },
      estimatedDuration: 1,
    },
    {
      id: "automated-score-bridge",
      title: "Automated Score Bridge",
      content: `The **score bridge** automatically connects the runner's internal quality metrics to the web dashboard's feedback system.

After every workflow run completes, the runner:
1. Computes agentic metrics (task_completion, step_efficiency, error_recovery, etc.)
2. Formats them as feedback score records
3. Batch-POSTs them to the web backend

This means **every run automatically gets quality scores** without any manual intervention — giving you a continuous quality signal across all your workflows.`,
      details: `The bridge is implemented as the Rust command \`push_agentic_scores_to_backend\`, which fires after run completion. It constructs a batch payload of \`FeedbackScoreCreate\` objects and POSTs to the web API. The storage strategy is PG-first with SQLite fallback — if the web backend is unreachable, scores are persisted locally and synced later. The summary panel aggregates these scores with mean, count, and distribution data.`,
      tips: [
        "If scores appear in the runner but not the dashboard, check your web backend connection",
      ],
      targetElement: {
        selector: '[data-tutorial-id="feedback-summary"]',
        highlightType: "border",
        position: "top",
      },
      estimatedDuration: 1,
    },
    {
      id: "run-cost-breakdown",
      title: "Per-Run Cost Breakdown",
      content: `Drill into any execution run to see a **detailed cost breakdown**:

- **Per-Model Costs** — see which models consumed the most budget (e.g., claude-sonnet-4-20250514 vs haiku)
- **Per-Action Costs** — which workflow actions were most expensive
- **Token Distribution** — input vs output token split for each call

This granularity helps you optimize costs by identifying which steps to simplify, which models to downgrade, or where caching could help.`,
      details: `The breakdown is served by the \`GET /execution-runs/{id}/cost-summary\` endpoint, returning an \`LLMCostSummary\` schema with per-model and per-action breakdowns. Each entry includes model name, provider, call count, total tokens, and total cost. The frontend renders this as a sortable table with proportional cost bars.`,
      tips: [
        "Look for actions that use expensive models for simple tasks — switching to a smaller model can cut costs dramatically",
        "High input token counts may indicate prompts that include too much context",
      ],
      targetElement: {
        selector: '[data-tutorial-id="run-cost-breakdown"]',
        highlightType: "border",
        position: "top",
      },
      estimatedDuration: 1,
    },
    {
      id: "prompt-versioning",
      title: "Prompt Versioning",
      content: `Every prompt change in Qontinui creates a **new immutable version**:

- **Version Numbers** — auto-incrementing (v1, v2, v3...) per prompt template
- **Content Hashing** — SHA256 hash ensures identical content is never duplicated
- **Diff View** — inline and side-by-side comparison between any two versions
- **Full History** — see who changed what, when, and why

This gives you a complete audit trail for every prompt in the system — essential for understanding why results changed.`,
      details: `The \`PromptTemplateVersion\` model stores the prompt content, version number (auto-incremented per template), content_hash (SHA256), and timestamps. The diff view uses \`unified_diff\` to generate line-by-line comparisons. Content hashing means that reverting to an earlier prompt does not create a duplicate version — the system recognizes the content already exists.`,
      tips: [
        "Always review the diff before deploying a prompt change to production workflows",
        "Use the version history to correlate prompt changes with quality score shifts",
      ],
      targetElement: {
        selector: '[data-tutorial-id="prompt-version-history"]',
        highlightType: "border",
        position: "left",
      },
      estimatedDuration: 1,
    },
    {
      id: "prompt-canary",
      title: "Prompt A/B Testing (Canary)",
      content: `The **canary system** lets you A/B test prompt variants in production:

- **Baseline** — the current production prompt version
- **Candidate** — the new prompt version being tested
- **Traffic Split** — configurable percentage of requests routed to the candidate
- **Statistical Evaluation** — p-value, confidence interval, and effect size metrics
- **Auto-Routing** — the pipeline loop calls \`should_use_candidate()\` to transparently route each request

When the candidate proves statistically superior, you can promote it to baseline with one click.`,
      details: `The canary is modeled by \`PromptTemplateCanary\` which references baseline and candidate version IDs, traffic split percentage, and evaluation criteria. Statistical analysis uses \`proportion_analysis\` for binary outcomes and standard t-tests for continuous scores, computing p-value, confidence interval bounds, and Cohen's d effect size. The Rust function \`resolve_prompt_with_canary_pg()\` handles prompt resolution at call time, checking for active canaries and routing based on the configured split.`,
      tips: [
        "Start with a small traffic split (10-20%) to limit blast radius from a bad candidate",
        "Wait for statistical significance before promoting — the dashboard shows when confidence is sufficient",
      ],
      targetElement: {
        selector: '[data-tutorial-id="prompt-canary-panel"]',
        highlightType: "border",
        position: "left",
      },
      estimatedDuration: 1,
    },
    {
      id: "dual-storage",
      title: "Dual Storage Architecture",
      content: `The observability system uses a **dual database architecture**:

**SQLite (Runner-Local)**
- Fast, works offline, no external dependencies
- Consumed by the meta-optimizer pipeline for local prompt tuning
- Stores traces, metrics, and feedback scores locally

**PostgreSQL (Web Backend)**
- Shared across all runners and team members
- Powers the web dashboard and API
- Enables cross-runner analytics and reporting

All command handlers follow a **PG-first with SQLite fallback** strategy — data is written to PostgreSQL when available, and always persisted locally.`,
      details: `The PostgreSQL integration uses 3 \`PgDb\` wrapper modules (for analytics, evaluation, and feedback) with 29 Clorinde-generated query definitions across 7 tables: llm_call_traces, feedback_scores, evaluation_datasets, dataset_items, evaluation_experiments, experiment_results, and prompt_template_versions. Clorinde generates type-safe Rust query functions from SQL definitions, avoiding the overhead of a full ORM. The SQLite schema mirrors these tables for local storage, with sync logic that reconciles on reconnection.`,
      tips: [
        "If you work offline frequently, your data will sync to PostgreSQL automatically when connectivity returns",
        "The meta-optimizer reads exclusively from SQLite to avoid network latency in the optimization loop",
      ],
      estimatedDuration: 1,
    },
    {
      id: "summary",
      title: "Observability System Complete",
      content: `You now understand the full Opik-inspired observability system in Qontinui.

**The 5 Pillars Recap:**
- **Tracing** — automatic capture of every LLM call with full telemetry
- **Evaluation** — dataset-driven experiments with LLM-as-judge metrics
- **Versioning** — immutable prompt versions with diff and audit trail
- **Feedback** — automated, judge, and manual quality scores
- **Cost Tracking** — per-run, per-model breakdowns with trend analysis

**Next Steps:**
- Create an evaluation dataset from your existing test cases
- Run an experiment to baseline your current prompt quality
- Check the cost trends page to understand your LLM spending
- Set up a canary to A/B test your next prompt improvement`,
      details: `For deeper reading, explore the Opik open-source project (github.com/comet-ml/opik) which inspired this system. Qontinui's implementation adapts Opik's concepts for a desktop-first, multi-runner architecture with offline support. The evaluation API is documented in the web backend's OpenAPI spec at /docs.`,
      resources: [
        {
          title: "Opik Documentation",
          url: "https://www.comet.com/docs/opik/",
          type: "documentation",
        },
        {
          title: "Qontinui Evaluation API",
          url: "/docs#evaluation",
          type: "api-reference",
        },
      ],
      tips: [
        "Start with cost tracking to get immediate value — no setup required",
        "Evaluation datasets are the highest-leverage investment for long-term prompt quality",
      ],
      estimatedDuration: 1,
    },
  ],
};
