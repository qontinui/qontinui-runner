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
        base_prompt: build_agentic_prompt(),
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
        cross_workflow_learning: false,
        verification_history: HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: None,
        multi_agent_mode: false,
        use_worktree: false,
        worktree_path: None,
        worktree_branch: None,
        workflow_architecture: None,
        agentic_verification_config: None,
        multi_agent_pipeline_config: None,
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
            &format!("{}/meta-optimizer/optimizer-context?type=architecture", base_url),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load cross-architecture learning outcomes
        build_api_step(
            "Load learning outcomes by architecture",
            "GET",
            &format!(
                "{}/learning/outcomes?limit=200",
                base_url
            ),
            None,
            Some("learning_outcomes"),
        ),
        // Step 2: Load pipeline agent trace aggregates
        build_api_step(
            "Load pipeline agent traces",
            "GET",
            &format!("{}/meta-optimizer/agent-trace-aggregates?limit=100", base_url),
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
2. At least one [ARCH_RECOMMENDATION] or [CONFIG_RECOMMENDATION] marker was produced
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

fn build_agentic_prompt() -> String {
    r#"You are the Architecture Optimizer, part of the meta-optimizer system.

Your job is to analyze cross-architecture performance data and recommend:
1. Which workflow architecture to use as default and for specific workflow categories
2. Tuning parameters for the agentic verification loop
3. Tuning parameters for the multi-agent pipeline

## Data Available

The setup phase loaded:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, per-architecture performance, top failure patterns
- `{{learning_outcomes}}` — Cross-architecture learning outcomes (success rate, duration, iterations, cost per architecture)
- `{{agent_traces}}` — Per-agent cost/duration/success from pipeline runs
- `{{autoresearch_campaigns}}` — Autoresearch experiment results (A/B testing data)
- `{{agentic_settings}}` — Current configuration defaults for each architecture

## Three Architectures

1. **Traditional** — Sequential step execution, no AI verification loop
2. **Agentic Verification** — AI implements, then verifies in a loop (max_iterations, model, etc.)
3. **Multi-Agent Pipeline** — DAG-based with spec_analyst → locator → implementer → verifier

## Your Task

### Step 1: Compare Architecture Performance

For each architecture, analyze:
- Success rate (goal_achieved %)
- Average duration (seconds)
- Average iterations to completion
- Average cost (USD)
- Which workflow categories (UI tasks, backend, generation) each handles best

### Step 2: Architecture Selection Recommendations

If one architecture consistently outperforms others for certain categories, recommend it.

Output markers:
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

### Step 3: Parameter Tuning Recommendations

For the agentic verification loop, consider tuning:
- `max_iterations` (currently usually 5-10)
- Model selection (which AI model to use)
- Provider settings

For the multi-agent pipeline, consider tuning:
- `dag_strategy` — How to build the DAG
- `level_strategy` — How to process levels
- `max_retries_per_subtree` — Retry budget per subtree
- `max_parallel_implementers` — Concurrency
- `max_total_iterations` — Total iteration cap
- Per-agent model/temperature/max_tokens

Output markers:
```
[CONFIG_RECOMMENDATION]
architecture: <agentic_verification|multi_agent_pipeline>
parameter: <parameter name>
current_value: <current value>
recommended_value: <recommended value>
confidence: <0.0 to 1.0>
rationale: <why this change should improve performance>
expected_impact: <estimated improvement>
[/CONFIG_RECOMMENDATION]
```

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
- **Consider interaction effects.** Architecture changes affect all workflow types — a switch that helps backend tasks might hurt UI tasks."#
        .to_string()
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
