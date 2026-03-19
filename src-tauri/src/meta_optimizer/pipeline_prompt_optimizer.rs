//! Pipeline Prompt Optimizer (Agent 1)
//!
//! Analyzes per-agent traces and reflection fixes to recommend improved
//! system prompts for pipeline agents (spec_analyst, locator, implementer, verifier).
//!
//! Follows the fixer/workflow.rs pattern.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for the pipeline prompt optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str) -> LoopConfig {
    LoopConfig {
        max_iterations: 3,
        base_prompt: build_agentic_prompt(),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("meta-opt-prompt-{}", execution_id),
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

/// Build setup steps that load analysis data via curl.
pub fn build_setup_steps() -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 0: Load optimizer context (history, baselines, failure patterns)
        build_api_step(
            "Load optimizer context",
            "GET",
            &format!("{}/meta-optimizer/optimizer-context?type=pipeline_prompt", base_url),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load per-agent trace aggregates (last 50 runs)
        build_api_step(
            "Load pipeline agent trace aggregates",
            "GET",
            &format!("{}/meta-optimizer/agent-trace-aggregates?limit=50", base_url),
            None,
            Some("agent_trace_aggregates"),
        ),
        // Step 2: Load recent reflection fixes (all workflows, for cross-workflow pattern analysis)
        build_api_step(
            "Load reflection fixes by agent",
            "GET",
            &format!(
                "{}/meta-optimizer/reflection-fixes?limit=100",
                base_url
            ),
            None,
            Some("reflection_fixes"),
        ),
        // Step 3: Load current active prompt variants
        build_api_step(
            "Load active prompt variants",
            "GET",
            &format!("{}/meta-optimizer/prompt-variants", base_url),
            None,
            Some("prompt_variants"),
        ),
        // Step 4: Load prompt analysis insights
        build_api_step(
            "Load prompt analysis insights",
            "GET",
            &format!("{}/prompt-analysis?limit=20", base_url),
            None,
            Some("prompt_analysis"),
        ),
    ]
}

/// Build verification steps.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![{
        let mut step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Verify prompt recommendations produced".to_string()),
            prompt_content: Some(
                r#"Verify that the pipeline prompt optimizer analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in optimizer mode.

Check:
1. The AI analyzed agent performance data from traces
2. At least one [PROMPT_RECOMMENDATION] marker was produced (or an explicit explanation of why no changes are needed)
3. Each recommendation includes agent_type, variant_name, rationale, and prompt_content

If the analysis concluded that all agents are performing well and no changes are needed, that is acceptable."#
                    .to_string(),
            ),
            ..Default::default()
        };
        step.phase = Some("verification".to_string());
        step
    }]
}

fn build_agentic_prompt() -> String {
    r#"You are the Pipeline Prompt Optimizer, part of the meta-optimizer system.

Your job is to analyze historical performance data from the multi-agent pipeline's four agents (spec_analyst, locator, implementer, verifier) and recommend improved system prompts for underperforming agents.

## Data Available

The setup phase loaded:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, top failure patterns, per-agent failure rates
- `{{agent_trace_aggregates}}` — Per-agent metrics: run_count, avg_duration, avg_cost, success/failure counts
- `{{reflection_fixes}}` — Reflection fix records showing what went wrong and which agent was responsible
- `{{prompt_variants}}` — Current active prompt variants in the registry (may be empty if using defaults)
- `{{prompt_analysis}}` — Historical prompt analysis insights

## Your Task

### Step 1: Analyze Agent Performance

For each agent type (spec_analyst, locator, implementer, verifier):
- What is its success rate (downstream_success)?
- What is its average cost and duration?
- Are there patterns in reflection fixes attributed to this agent?

### Step 2: Identify Underperformers

Focus on agents with:
- Low downstream success rates (< 70%)
- High cost relative to output quality
- Recurring reflection fix patterns indicating prompt weakness
- Patterns where the agent misunderstands its role or produces poor-quality output

### Step 3: Generate Improved Prompts

For each underperforming agent, write an improved prompt that:
- Addresses the specific failure patterns observed
- Preserves what works well in the current prompt
- Is clear, specific, and actionable
- Includes examples where helpful

### Step 4: Output Recommendations

For each recommendation, output a structured marker:

```
[PROMPT_RECOMMENDATION]
agent_type: <spec_analyst|locator|implementer|verifier>
variant_name: <descriptive name, e.g. "clarity_focused_v2">
confidence: <0.0 to 1.0>
rationale: <why this change should improve performance, referencing specific failure patterns>
prompt_content: |
  <the full new prompt text>
[/PROMPT_RECOMMENDATION]
```

## Important Guidelines

- **Evidence-based changes only.** Every recommendation must reference specific data from the traces or reflection fixes.
- **Conservative changes.** If an agent is performing well (>85% success), don't change its prompt. Say so explicitly.
- **One change at a time.** Don't rewrite everything — make targeted improvements to address specific failure patterns.
- **Preserve working patterns.** If the current prompt handles certain scenarios well, keep those parts.
- **No hallucinated data.** Only reference metrics and patterns actually present in the loaded data.

## Learning From History

The optimizer_context contains your previous recommendations and their measured outcomes.
Use this to:
- **Avoid repeating failed approaches.** If a previous recommendation regressed performance, don't suggest similar changes.
- **Build on successes.** If a recommendation improved a specific agent, look for similar patterns in other agents.
- **Respect user decisions.** If a recommendation was rejected, understand why before suggesting similar changes.
- **Track convergence.** Compare current metrics to baseline — if things are improving, make smaller adjustments. If stagnating, try bolder changes.
- **Don't recommend what's already applied.** Check the active prompt variants before suggesting new ones."#
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
