/**
 * PRM Training Pipeline Tutorial
 *
 * Informational tutorial explaining the Process Reward Model training pipeline —
 * how the runner's own execution results become labeled training data, how the
 * Python service trains a fast local scorer, and how it integrates with the
 * evaluation engine (Plan 12) and tree search (Plan 13) to replace expensive
 * LLM-as-a-judge calls.
 */

import type { Tutorial } from "../../../types/tutorial";

export const prmTrainingPipelineTutorial: Tutorial = {
  id: "prm-training-pipeline",
  title: "Process Reward Model Training Pipeline",
  description:
    "Understand how the runner trains domain-adapted Process Reward Models — using its own execution feedback as ground-truth labels — to replace expensive LLM-as-a-judge calls with fast local inference during step evaluation and tree search.",
  duration: "15 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "help",
  category: "Architecture",
  tags: ["prm", "training", "architecture", "evaluation", "machine-learning", "featured"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand the FoVer insight: why the runner IS a formal verifier that generates free labeled data",
    "Know the 3-tier scoring architecture and where the PRM sits (Tier 3)",
    "Understand the data pipeline: export, annotate, filter, split",
    "Know how ThinkPRM-style training works with critique + score format",
    "Understand PURE min-form trajectory scoring (weakest step = overall quality)",
    "Know the serving architecture: FastAPI + sglang/vLLM with model hot-swap",
    "Understand the continuous retraining loop and quality gates",
  ],
  steps: [
    {
      id: "intro-problem",
      title: "The Cost Problem",
      content: `The evaluation engine (Plan 12) scores verification steps across 6 dimensions. During tree search (Plan 13), each candidate workflow needs scoring. The math gets expensive fast:

**5 candidates x 8 steps x 3 judge dimensions = 120 LLM calls per workflow**

At 100-500ms and $0.01-0.05 per call, that's 12-60 seconds and $1.20-6.00 per generation. For a system that generates hundreds of workflows daily, this doesn't scale.

The **Process Reward Model** (PRM) replaces those 120 LLM-as-a-judge calls with 40 fast local inference calls (1 PRM call per step replacing 3 judge calls) — each taking 10-50ms at zero marginal cost.`,
      estimatedDuration: 1.5,
      tips: [
        "Tier 1 (deterministic checks) stays — it's free and catches obvious issues in < 1ms",
        "The PRM doesn't need to be perfect — it just needs to rank candidates well enough for tree search",
      ],
    },
    {
      id: "fover-insight",
      title: "The FoVer Insight: The Runner IS a Verifier",
      content: `The key insight from the FoVer paper: **formal verification results are free ground-truth labels for training reward models.**

Every time the runner executes a verification step and it passes or fails, that's a labeled training example:

| Signal | Training Label |
|--------|---------------|
| Step passed on first attempt, workflow passed | **1.0** (excellent step) |
| Step passed first attempt, workflow failed | **0.7** (step ok, others failed) |
| Step failed, then fixed and passed | **0.2** (original was flawed) |
| Step failed, never fixed | **0.0** (bad step) |

The **fixer diffs** are also gold — they tell the PRM *exactly what was wrong* with a failed step, providing natural language critique training signal.`,
      estimatedDuration: 1.5,
      details: `**Why this is powerful:**

Traditional PRM training requires expensive human annotations or Monte Carlo sampling (generate thousands of rollouts to estimate step quality). The runner generates this signal for free as a side effect of normal operation.

The ThinkPRM paper showed that just **1,000 examples** are sufficient to train a strong verifier when the training format includes a critique chain-of-thought before the score. With hundreds of workflow runs generating 5-15 step examples each, the runner accumulates enough training data within weeks of normal use.

**Cross-domain transfer** (from FoVer): PRMs trained on one verification domain (e.g., build steps) transfer surprisingly well to adjacent domains (API testing, UI automation) — the model learns general verification quality patterns, not just domain-specific rules.`,
      tips: [
        "The runner has been accumulating this data in generation_pipeline_artifacts since day one",
        "Check current data volume: GET /prm/stats returns how many labeled examples are available",
      ],
    },
    {
      id: "three-tier-architecture",
      title: "Three-Tier Scoring Architecture",
      content: `The evaluation engine uses a tiered scoring strategy. Each tier adds depth but costs more:

**Tier 1 — Deterministic Checks (< 1ms, free)**
Pattern matching, syntax validation, and structural rules. Catches obvious issues like missing required flags, empty commands, or known anti-patterns.

**Tier 2 — LLM-as-a-Judge (100-500ms, $0.01-0.05)**
Composable judge pipeline: EntailmentJudge, DeterminismJudge, SpecificityJudge. High accuracy but slow and expensive.

**Tier 3 — PRM Scorer (10-50ms, free after training)**
Trained on the runner's own execution data. Produces a critique + quality score in a single forward pass. Replaces 3 Tier 2 judges.

The strategy is configurable per context:
- \`FastOnly\` — Tier 1 only (used during generation for quick gating)
- \`Standard\` — Tiers 1 + 2 (used for final evaluation)
- \`Full\` — Tiers 1 + 2 + 3 (used when PRM is available)`,
      estimatedDuration: 1,
      tips: [
        "The scoring strategy is set per-request via the /evaluation/workflow API",
        "Tree search (Plan 13) always uses Full strategy when the PRM service is running",
      ],
    },
    {
      id: "data-export",
      title: "Phase 1: Data Export from the Runner",
      content: `The Rust-side export lives in two places:

**\`workflow_generation/prm_export.rs\`** — SQLite export logic (legacy)
**\`database/pg/tiered_info.rs\`** — PostgreSQL export (production)

Both JOINs \`task_runs\` with \`generation_pipeline_artifacts\` to extract:
- **Step definitions** from the final workflow JSON
- **Pass/fail labels** from \`verification_iterations\`
- **Fixer diffs** from \`fixer_snapshots\` (what the fixer changed to fix a failed step)
- **Acceptance criteria** from \`specification_criteria\`
- **Domain** (Compilation, API, UI, etc.) from the pipeline category

The export produces one example per step per iteration — a step that failed twice then passed generates 3 examples (2 negative, 1 positive).`,
      estimatedDuration: 1,
      details: `**HTTP endpoints:**

\`\`\`
GET /prm/export?min_runs=10&format=jsonl  — Full export (JSONL or JSON)
GET /prm/stats                            — Lightweight count query
\`\`\`

**Export JSON shape** (one per line in JSONL):
\`\`\`json
{
  "run_id": "abc-123",
  "step_index": 2,
  "step_definition": { "step_type": "command", "name": "check build", ... },
  "criterion": { "id": "build-passes", "description": "Build succeeds" },
  "workflow_context": "Verify React app builds",
  "execution_result": "failed",
  "iteration": 0,
  "fixer_diff": "- npm build\\n+ npm run build",
  "final_outcome": "step_fixed_later",
  "domain": "compilation"
}
\`\`\`

The SQL JOIN prioritizes \`task_run_id\` matches and falls back to \`workflow_id\` when the artifact has no direct task_run link. This prevents duplicate rows.`,
    },
    {
      id: "data-pipeline",
      title: "Phase 2: Python Data Pipeline",
      content: `The \`qontinui-prm\` Python project processes raw exports into training-ready datasets through four stages:

**1. Extractor** (\`data/extractor.py\`)
Parses runner JSONL/JSON into \`PrmTrainingSample\` objects. Computes quality labels from execution results (the 0.0-1.0 scoring from the FoVer insight). Supports both file and HTTP extraction.

**2. Annotator** (\`data/annotator.py\`)
Generates ThinkPRM-style critique annotations. Two modes:
- **LLM critiques** — Uses Claude to analyze each step for DETERMINISM, ENTAILMENT, SPECIFICITY, EXECUTABILITY
- **Synthetic critiques** — Rule-based fallback when no LLM is available (sufficient for initial training)

**3. Filter** (\`data/filter.py\`)
Quality filtering: removes empty/placeholder samples, deduplicates by content fingerprint, removes critique-label contradictions, and balances positive/negative ratios (max 80/20 split).

**4. Splitter** (\`data/splitter.py\`)
Stratified train/val/test split (80/10/10) preserving domain proportions.`,
      estimatedDuration: 1.5,
      tips: [
        "Run the full pipeline: qontinui-prm prepare export.jsonl --annotate -o ./data/processed",
        "Synthetic critiques are good enough for initial training — LLM critiques help for refinement",
      ],
    },
    {
      id: "think-prm-training",
      title: "Phase 3: ThinkPRM-Style Training",
      content: `The training format follows ThinkPRM's insight: **models that reason before scoring are dramatically better verifiers.** With just 1K critique-then-score examples, a 7B model matches or exceeds 70B models trained on pure labels.

**Training format** (SFT on chat conversations):
\`\`\`
System: You are a verification step quality evaluator...
User: [context] + [criterion] + [step] + "Evaluate this step."
Assistant: [detailed critique covering 4 dimensions]
           **Quality Score:** 0.8
\`\`\`

**Model:** Qwen-2.5-7B-Instruct with LoRA adapters
**Parameters:** r=16, alpha=32, targeting q/k/v/o projections
**Training:** 3 epochs, batch_size=4, gradient accumulation=4, lr=2e-5

LoRA keeps the base model frozen and only trains ~2% of parameters (160M trainable out of 7.6B total), making training fast and memory-efficient.`,
      estimatedDuration: 1.5,
      details: `**Why Qwen-2.5-7B?**

- Strong instruction-following baseline for critique generation
- 7B fits comfortably on a single consumer GPU (RTX 5090 with 32GB VRAM)
- LoRA adapter is only ~30MB — easy to version, swap, and deploy
- Supports BF16 natively for efficient inference

**Quality gate:** After training, the model is evaluated on the held-out test set. Four metrics must pass:
- **Pearson correlation** — predicted vs actual scores
- **Binary accuracy** — pass/fail classification (threshold: 0.7)
- **F1 score** — balanced precision/recall
- **Ranking accuracy** — pairwise: does the model rank better steps higher?

If binary accuracy < 0.7, the model should not be deployed. Training with more data or adjusting hyperparameters is needed.`,
      tips: [
        "Run training: qontinui-prm train --data-dir ./data/processed --epochs 3",
        "Training on 1K examples takes ~20 minutes on an RTX 5090",
        "The LoRA adapter saves to ./models/qontinui-prm/ by default",
      ],
    },
    {
      id: "pure-min-form",
      title: "PURE Min-Form: Trajectory Scoring",
      content: `How do you combine per-step scores into a workflow-level quality score? The PURE framework's answer: **take the minimum.**

**Trajectory score = min(step scores)**

This is called "min-form credit assignment" and it has a powerful property: **the weakest step determines overall workflow quality.** A workflow with 7 perfect steps and 1 terrible step is still a bad workflow.

This directly maps to verification reality — if one verification step is non-deterministic or doesn't actually prove its criterion, the entire workflow's reliability is compromised regardless of how good the other steps are.

The PRM service exposes this via \`POST /v1/score/trajectory\`:
\`\`\`json
{
  "trajectory_score": 0.3,
  "step_scores": [...],
  "weakest_step_index": 4,
  "weakest_step_critique": "DETERMINISM: This prompt-based step..."
}
\`\`\`

Tree search uses this to rank candidates — the one with the highest minimum is selected.`,
      estimatedDuration: 1,
      tips: [
        "The weakest_step_critique is fed back to the fixer for targeted repair",
        "PURE min-form is also used by Plan 12's aggregation module for the same reason",
      ],
    },
    {
      id: "serving-architecture",
      title: "Phase 4: Serving via FastAPI",
      content: `The trained PRM is served as an HTTP service at \`localhost:8400\`:

| Endpoint | Purpose | Latency |
|----------|---------|---------|
| \`POST /v1/score\` | Score a single step | 10-50ms |
| \`POST /v1/score/batch\` | Score multiple steps | 30-150ms |
| \`POST /v1/score/trajectory\` | PURE min-form trajectory | 50-200ms |
| \`GET /health\` | Health check (GPU, model status) | < 1ms |
| \`POST /v1/reload\` | Hot-swap model after retraining | 5-10s |

**Request format** (what the runner's PrmClient sends):
\`\`\`json
{
  "step_text": "Step Type: command\\nName: check build\\nCommand: npm run build",
  "criterion_text": "Criterion: Build succeeds with no errors\\nMethod: command",
  "context_text": "Verify React application builds correctly"
}
\`\`\`

The response includes both a numeric score and a natural language critique — the critique is valuable for the fixer to understand *why* a step scored poorly.`,
      estimatedDuration: 1,
      details: `**Deployment options:**

1. **Docker** — \`qontinui-prm/docker/docker-compose.yml\` with NVIDIA GPU passthrough
2. **Direct** — \`qontinui-prm serve --model-path ./models/qontinui-prm --port 8400\`
3. **sglang/vLLM** — For production, serve the base model + LoRA adapter through sglang for optimized batching

**Auto-discovery:** The runner's evaluation API defaults to \`QONTINUI_PRM_URL\` env var, falling back to \`http://localhost:8400\`. The PrmClient in \`evaluation/prm_client.rs\` gracefully falls back if the service is unavailable — evaluation continues with Tiers 1+2 only.`,
    },
    {
      id: "prm-client-integration",
      title: "PrmClient: Rust-Side Integration",
      content: `The PrmClient (\`evaluation/prm_client.rs\`) bridges the runner's Rust evaluation engine with the Python scoring service:

\`\`\`
Runner evaluation loop
  -> Tier 1: deterministic checks (inline Rust)
  -> Tier 2: LLM judges (HTTP to Claude/OpenAI)
  -> Tier 3: PrmClient::score_step_sync() -> HTTP to localhost:8400
\`\`\`

**Key methods:**
- \`score_step_sync(step, criteria)\` — Single step scoring
- \`score_batch_sync(steps, criteria)\` — Batch for tree search efficiency
- \`score_trajectory_sync(steps, criteria)\` — PURE min-form trajectory
- \`health()\` — Check if the PRM service is running

The client formats step JSON into the text fields the Python server expects (\`format_step_text\`, \`format_criteria_text\`), handling the Rust-to-Python type boundary.

When the PRM service is unavailable, the evaluation engine silently falls back to Tier 2 LLM judges — no disruption to the workflow.`,
      estimatedDuration: 1,
      tips: [
        "The PrmClient uses reqwest::blocking with a 10-second timeout",
        "Batch scoring is used by Plan 13's tree search to score all candidates in one call",
      ],
    },
    {
      id: "continuous-retraining",
      title: "Continuous Retraining Loop",
      content: `The PRM improves over time as the runner accumulates more execution data. The retraining loop:

\`\`\`
1. Runner executes workflows -> new labeled data accumulates
2. qontinui-prm retrain --export-url http://localhost:1420/prm/export
3. Fetches latest data, generates synthetic critiques
4. Filters + splits, trains on new data
5. POST /v1/reload hot-swaps the model weights
6. PRM service immediately serves updated model
\`\`\`

This can be automated as a weekly cron job. The \`retrain\` CLI command handles the entire pipeline: fetch data, prepare, train, and hot-swap.

**Quality gate:** If training loss exceeds 2.0, the command warns that data quality should be investigated. The held-out test set evaluation runs automatically, and the model shouldn't be deployed if binary accuracy drops below 0.7.`,
      estimatedDuration: 1,
      tips: [
        "Run manually: qontinui-prm retrain --min-new-examples 100",
        "Hot-swap takes 5-10 seconds — requests during swap return 503 and retry",
        "The learning_orchestrator.rs has the retrain URLs configured in LearningConfig",
      ],
    },
    {
      id: "data-flow-summary",
      title: "End-to-End Data Flow",
      content: `Here's the complete lifecycle:

**Generate:** Runner executes workflow -> steps pass/fail -> fixer diffs recorded
**Export:** \`GET /prm/export\` -> step definitions + labels + diffs as JSONL
**Prepare:** \`qontinui-prm prepare\` -> extract, annotate, filter, split
**Train:** \`qontinui-prm train\` -> LoRA fine-tune on critique + score format
**Serve:** \`qontinui-prm serve\` -> FastAPI on port 8400
**Score:** PrmClient calls \`/v1/score\` during evaluation (Tier 3)
**Improve:** \`qontinui-prm retrain\` -> fresh data, hot-swap model

The feedback loop is self-reinforcing: better PRM scores lead to better step selection in tree search, which produces higher-quality workflows, which generate better training data for the next PRM version.

**Minimum viable training:** ~1,000 completed runs with pipeline artifacts. Check your current volume with \`GET /prm/stats\`.`,
      estimatedDuration: 1,
    },
    {
      id: "project-structure",
      title: "Project Layout",
      content: `**Rust (in qontinui-runner):**

| File | Role |
|------|------|
| \`workflow_generation/prm_export.rs\` | SQLite export types & logic |
| \`evaluation/prm_client.rs\` | HTTP client for PRM service |
| \`mcp/prm_export.rs\` | \`GET /prm/export\` + \`GET /prm/stats\` routes |
| \`mcp/step_evaluation_api.rs\` | Evaluation API with PRM URL auto-discovery |
| \`database/pg/tiered_info.rs\` | PostgreSQL export queries |
| \`learning_orchestrator.rs\` | LearningConfig with PRM service URL |

**Python (qontinui-prm/):**

| Directory | Contents |
|-----------|----------|
| \`src/qontinui_prm/data/\` | extractor, annotator, filter, splitter |
| \`src/qontinui_prm/training/\` | config, trainer, evaluate, reward |
| \`src/qontinui_prm/serving/\` | FastAPI server, models, health |
| \`src/qontinui_prm/cli.py\` | CLI: prepare, train, serve, retrain |
| \`docker/\` | Dockerfile + docker-compose.yml |
| \`configs/\` | training_default.yaml, serving_default.yaml |

**52 tests** cover the Python pipeline (extractor, filter, annotator, splitter, evaluate, reward, server, CLI).`,
      estimatedDuration: 1,
      tips: [
        "Install the Python project: cd qontinui-prm && poetry install",
        "For GPU training: poetry install --with ml",
        "For serving: poetry install --with serving",
      ],
    },
  ],
};
