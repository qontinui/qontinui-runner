//! Architecture Optimizer (Agent 2)
//!
//! Compares all three workflow architectures (traditional, agentic_verification,
//! multi_agent_pipeline) and recommends best per workflow type. Also tunes
//! parameters within each architecture.
//!
//! Follows the fixer/workflow.rs pattern.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for the architecture optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str) -> LoopConfig {
    LoopConfig {
        max_iterations: 3,
        base_prompt: build_agentic_prompt(&crate::mcp::types::get_self_base_url_from_env()),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("meta-opt-arch-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true,
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade
        enable_sweep: false,
        max_sweep_iterations: 0,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: HashMap::new(),
        reflection_mode: false,
        provider_override: None,
        model_override: None,
        model_overrides: HashMap::new(),
        stage_index: None,
        max_sessions: Some(3),
        auto_run_generated: false,
        approval_gate: false,
        max_context_tokens: 200_000,
        enforce_token_budget: false,
        cross_workflow_learning: false,
        verification_history: HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: None,
        multi_agent_mode: false,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        iteration_diffs: Vec::new(),
    }
}

/// Build setup steps that load cross-architecture analysis data.
pub fn build_setup_steps() -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 0: Load optimizer context (history, baselines, failure patterns)
        build_api_step(
            "Load optimizer context",
            "GET",
            &format!(
                "{}/meta-optimizer/optimizer-context?type=architecture",
                base_url
            ),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load cross-architecture learning outcome summaries (L0 — counts by status)
        build_api_step(
            "Load learning outcome summaries (L0)",
            "GET",
            &format!("{}/learning/outcomes?limit=200&tier=l0", base_url),
            None,
            Some("learning_outcomes"),
        ),
        // Step 2: Load pipeline agent trace summaries (L0 — agent_type + count + success_pct)
        build_api_step(
            "Load pipeline agent trace summaries (L0)",
            "GET",
            &format!(
                "{}/meta-optimizer/agent-trace-aggregates?limit=100&tier=l0",
                base_url
            ),
            None,
            Some("agent_traces"),
        ),
        // Step 3: Load autoresearch experiment results
        build_api_step(
            "Load autoresearch experiments",
            "GET",
            &format!("{}/autoresearch/campaigns?limit=10", base_url),
            None,
            Some("autoresearch_campaigns"),
        ),
        // Step 4: Load current agentic settings
        build_api_step(
            "Load agentic settings",
            "GET",
            &format!("{}/settings/agentic", base_url),
            None,
            Some("agentic_settings"),
        ),
        // Step 5: Load iteration history patterns (L0 — approach patterns from agentic loops)
        build_api_step(
            "Load iteration history patterns (L0)",
            "GET",
            &format!(
                "{}/meta-optimizer/iteration-history?limit=100&tier=l0",
                base_url
            ),
            None,
            Some("iteration_history"),
        ),
    ]
}

/// Build verification steps.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![{
        let mut step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Verify architecture recommendations produced".to_string()),
            prompt_content: Some(
                r#"Verify that the architecture optimizer analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in optimizer mode.

Check:
1. The AI analyzed cross-architecture performance data
2. At least one [ARCH_RECOMMENDATION], [CONFIG_RECOMMENDATION], or [ARCH_FINDING] marker was produced
   (or an explicit explanation of why no changes are needed)
3. Each recommendation includes evidence and rationale

If the current architecture settings are optimal, that is acceptable."#
                    .to_string(),
            ),
            ..Default::default()
        };
        step.phase = Some("verification".to_string());
        step
    }]
}

fn build_agentic_prompt(base_url: &str) -> String {
    let prompt = r#"You are the Architecture Optimizer, part of the meta-optimizer system.

Your job is to analyze cross-architecture performance data and produce TWO types of output:
1. **Actionable config recommendations** — for parameters that can actually be changed via settings
2. **Advisory findings** — for insights that require code changes or human decisions

## CRITICAL: What You Can and Cannot Change

### You CAN recommend changes to (via CONFIG_RECOMMENDATION):
- The `dev_mode` settings JSON operational knobs:
  - `meta_optimizer_enabled` (bool)
  - `meta_optimizer_threshold` (int)
  - `fixer_enabled` (bool)
- Generation rules (via the Generation Template Optimizer — not your job)

### You CANNOT change via CONFIG_RECOMMENDATION:
- `max_iterations` — this is set per-workflow in LoopConfig, not in global settings
- `dag_strategy`, `level_strategy`, `max_retries_per_subtree` — these are pipeline config, not settings
- `max_parallel_implementers`, `max_total_iterations` — pipeline config, not settings
- Architecture selection — this is per-workflow, not a global default
- Model selection — this is per-provider in AI settings, not a simple key-value change
- Any parameter with a key like `agentic_verification.X` or `multi_agent_pipeline.X` — these keys DO NOT EXIST in the settings system

If your analysis reveals an important insight about any of these non-configurable parameters, output it as an [ARCH_FINDING], NOT as a [CONFIG_RECOMMENDATION].

## Data Available (Tiered Context — L0 Summaries)

The setup phase loaded **L0 summaries** where applicable:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, per-architecture performance, top failure patterns
- `{{learning_outcomes}}` — **L0 summary**: status, run_count, avg_duration_secs, avg_iterations (grouped by status)
- `{{agent_traces}}` — **L0 summary**: agent_type, run_count, success_pct (one row per agent type)
- `{{autoresearch_campaigns}}` — Autoresearch experiment results (A/B testing data)
- `{{agentic_settings}}` — Current configuration defaults for each architecture
- `{{iteration_history}}` — **L0 summary**: approach patterns from agentic verification loops (avg iterations, confidence trends, most common approaches tried, grouped by run status)

## Drill-Down: L1/L2 Detail Endpoints

If the L0 summaries reveal issues, drill into details:

**Learning Outcomes — L1 (core fields per outcome):**
```bash
curl -s '{{base_url}}/learning/outcomes?tier=l1&limit=50'
```
Returns: id, task_id, status, duration_secs, iterations, workflow_architecture, error_type, created_at.

**Learning Outcomes — L2 (full records including tools, files, error messages):**
```bash
curl -s '{{base_url}}/learning/outcomes?tier=l2&limit=20'
```
Returns: full records with strategy, tools_used, files_modified, error_message.

**Iteration History — L1 (per-run iteration sequences):**
```bash
curl -s '{{base_url}}/meta-optimizer/iteration-history?tier=l1&status=failed&limit=20'
```
Returns: task_run_id, approaches tried, iteration count, confidence trajectory per run.

**Iteration History — L2 (full iteration data including files touched):**
```bash
curl -s '{{base_url}}/meta-optimizer/iteration-history?tier=l2&limit=10'
```
Returns: complete iteration entries with approach, result, files_touched, confidence_delta.

**Agent Traces — L1 (core fields per trace):**
```bash
curl -s '{{base_url}}/meta-optimizer/agent-trace-aggregates?tier=l1&agent_type=<AGENT_TYPE>&limit=20'
```

**Agent Traces — L2 (full aggregated statistics):**
```bash
curl -s '{{base_url}}/meta-optimizer/agent-trace-aggregates?tier=l2&limit=100'
```
Returns: avg_duration_ms, avg_cost_usd, avg_tokens_in/out per agent type.

Only drill into L1/L2 when the L0 summaries show a problem worth investigating.

## Three Architectures

1. **Traditional** — Sequential step execution, no AI verification loop
2. **Agentic Verification** — AI implements, then verifies in a loop (max_iterations, model, etc.)
3. **Multi-Agent Pipeline** — DAG-based with spec_analyst → locator → implementer → verifier

## Your Task

### Step 1: Analyze the Data

For each architecture, analyze:
- Success rate (goal_achieved %)
- Average duration (seconds)
- Average iterations to completion
- Average cost (USD)
- Which workflow categories (UI tasks, backend, generation) each handles best

### Step 2: Output Findings (for insights that need code changes)

For important insights that cannot be implemented via config changes, output:

```
[ARCH_FINDING]
category: <iteration_tuning|architecture_selection|failure_pattern|data_gap>
title: <short descriptive title>
severity: <critical|important|informational>
finding: <what the data shows>
evidence: <specific numbers from the data>
suggested_action: <what a developer should do about this>
[/ARCH_FINDING]
```

Examples of correct findings:
- "90% of failures occur when max_iterations=0" → ARCH_FINDING (max_iterations is per-workflow LoopConfig)
- "Pipeline runs with dag_strategy=parallel outperform sequential by 40%" → ARCH_FINDING (dag_strategy is pipeline config)
- "Agentic verification succeeds 2x more often for UI tasks" → ARCH_FINDING (architecture selection is per-workflow)

### Step 3: Output Config Recommendations (ONLY for actually configurable parameters)

ONLY output [CONFIG_RECOMMENDATION] if there is a real settings key that can be changed via `db.set_setting(key, value)`. Currently the only configurable parameters via settings are operational knobs in the dev_mode JSON.

If you have no real config changes to recommend, output ZERO [CONFIG_RECOMMENDATION] markers. That is perfectly acceptable.

```
[CONFIG_RECOMMENDATION]
architecture: <agentic_verification|multi_agent_pipeline>
parameter: <parameter name — must be an actual settings key>
current_value: <current value>
recommended_value: <recommended value>
confidence: <0.0 to 1.0>
rationale: <why this change should improve performance>
expected_impact: <estimated improvement>
[/CONFIG_RECOMMENDATION]
```

### Step 4: Architecture Selection Recommendations (if supported by data)

If one architecture consistently outperforms others for certain categories, recommend it as an ARCH_RECOMMENDATION. Note: these are advisory — architecture selection is per-workflow in LoopConfig and cannot be changed globally via settings.

```
[ARCH_RECOMMENDATION]
workflow_category: <all|ui_tasks|backend|generation|testing>
recommended_architecture: <traditional|agentic_verification|multi_agent_pipeline>
current_architecture: <what is currently used>
confidence: <0.0 to 1.0>
rationale: <why, with specific metrics>
evidence: <key data points>
[/ARCH_RECOMMENDATION]
```

## Quality Gate

- **Do NOT produce recommendations below 60% confidence.** If you're not sure, produce a finding instead.
- **Do NOT produce CONFIG_RECOMMENDATION for parameters that aren't in the settings system.** Output findings instead.
- **Do NOT duplicate previous recommendations.** Check the optimizer_context for what was already recommended.
- **If the data is insufficient, say so explicitly and produce no recommendations.** A clear "insufficient data" analysis is more valuable than guessing.

## Important Guidelines

- **Data-driven only.** Base recommendations on observed metrics, not intuition.
- **Statistical significance.** Don't recommend changes based on < 5 data points.
- **Conservative tuning.** Prefer small parameter adjustments over dramatic changes.
- **Consider trade-offs.** Faster may cost more. More iterations may improve quality. Document trade-offs.
- **No hallucinated data.** Only reference metrics actually present in the loaded data.

## Learning From History

The optimizer_context contains your previous recommendations and their measured outcomes.
Use this to:
- **Don't flip-flop.** If you recommended switching to architecture A last time and it regressed, don't recommend it again without new evidence.
- **Measure before changing.** Check the baseline vs current delta before recommending architecture switches.
- **Distinguish noise from signal.** Small sample sizes (< 10 runs per architecture) don't justify switching defaults.
- **Consider interaction effects.** Architecture changes affect all workflow types — a switch that helps backend tasks might hurt UI tasks."#;
    prompt.replace("{{base_url}}", base_url)
}

/// Helper: Build a command step that makes an HTTP request via curl.
fn build_api_step(
    name: &str,
    method: &str,
    url: &str,
    body: Option<&str>,
    output_variable: Option<&str>,
) -> ExecutionStepConfig {
    let curl_cmd = if let Some(body_str) = body {
        format!(
            "curl -s -X {} -H \"Content-Type: application/json\" -d '{}' '{}' || echo '{{}}'",
            method, body_str, url
        )
    } else {
        format!("curl -s -X {} '{}' || echo '{{}}'", method, url)
    };

    let extract = output_variable.map(|var| {
        let mut map = HashMap::new();
        map.insert(var.to_string(), "$".to_string());
        map
    });

    let mut step = ExecutionStepConfig {
        step_type: "command".to_string(),
        command_mode: Some("shell".to_string()),
        name: Some(name.to_string()),
        shell_command: Some(curl_cmd),
        extract,
        ..Default::default()
    };
    step.phase = Some("setup".to_string());
    step.run_on_subsequent_iterations = Some(false);
    step
}
