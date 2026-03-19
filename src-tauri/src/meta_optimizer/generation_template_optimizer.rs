//! Generation Template Optimizer (Agent 3)
//!
//! Improves workflow generation rules used by builder, verification, and
//! hardener agents in generator.rs.
//!
//! Follows the fixer/workflow.rs pattern.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for the generation template optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str) -> LoopConfig {
    LoopConfig {
        max_iterations: 3,
        base_prompt: build_agentic_prompt(),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("meta-opt-gen-{}", execution_id),
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

/// Build setup steps that load generation feedback and rules data.
pub fn build_setup_steps() -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 0: Load optimizer context (history, baselines, quality trends)
        build_api_step(
            "Load optimizer context",
            "GET",
            &format!("{}/meta-optimizer/optimizer-context?type=generation_template", base_url),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load workflow generation feedback (edits, deletes, ratings)
        build_api_step(
            "Load generation feedback",
            "GET",
            &format!(
                "{}/workflow-generation/feedback?limit=100",
                base_url
            ),
            None,
            Some("generation_feedback"),
        ),
        // Step 2: Load current active generation rules
        build_api_step(
            "Load generation rules",
            "GET",
            &format!("{}/generation-rules?status=active", base_url),
            None,
            Some("generation_rules"),
        ),
        // Step 3: Load prompt analysis insights
        build_api_step(
            "Load prompt analysis",
            "GET",
            &format!("{}/prompt-analysis?limit=20", base_url),
            None,
            Some("prompt_analysis"),
        ),
        // Step 4: Load recent task run outcomes for context
        build_api_step(
            "Load recent task outcomes",
            "GET",
            &format!("{}/learning/outcomes?limit=50", base_url),
            None,
            Some("recent_outcomes"),
        ),
    ]
}

/// Build verification steps.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![{
        let mut step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Verify generation rule recommendations".to_string()),
            prompt_content: Some(
                r#"Verify that the generation template optimizer analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in optimizer mode.

Check:
1. The AI analyzed generation feedback and current rules
2. At least one [RULE_RECOMMENDATION] marker was produced (or an explicit explanation of why no changes are needed)
3. Each recommendation includes action, agent, section, title, content, and rationale

If all current rules are effective, that is acceptable."#
                    .to_string(),
            ),
            ..Default::default()
        };
        step.phase = Some("verification".to_string());
        step
    }]
}

fn build_agentic_prompt() -> String {
    r#"You are the Generation Template Optimizer, part of the meta-optimizer system.

Your job is to analyze workflow generation feedback and recommend improvements to the generation rules used by the builder, verification, and hardener agents.

## Data Available

The setup phase loaded:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, generation quality trends
- `{{generation_feedback}}` — User feedback on generated workflows: edits, deletes, ratings, and which fields were commonly edited
- `{{generation_rules}}` — Current active generation rules for all agents (schema_context, hardener, verification)
- `{{prompt_analysis}}` — Historical prompt analysis insights
- `{{recent_outcomes}}` — Recent task run outcomes (success/failure patterns)

## Generation Rules System

Rules are organized by:
- **agent**: `schema_context`, `hardener`, `verification`
- **section**: `important_rules`, `verification_quality`, `conversion_rules`, etc.
- **provenance**: `seed` (original), `reflection` (from reflection fixes), `auto_insight` (auto-generated)
- **status**: `active`, `disabled`, `superseded`

## Your Task

### Step 1: Analyze Feedback Patterns

Look for:
- Which fields are users editing most frequently? (indicates generation is getting them wrong)
- Which generated workflows are being deleted? (indicates fundamental quality issues)
- What ratings are users giving? Are there patterns in low-rated workflows?
- Are there common edit patterns that could be captured as rules?

### Step 2: Evaluate Current Rules

For each active rule:
- Is it still relevant? (evidence in recent outcomes)
- Is it being followed? (check against recent feedback)
- Could it be improved or made more specific?
- Are there any conflicting rules?

### Step 3: Generate Recommendations

Output markers for each recommendation:

```
[RULE_RECOMMENDATION]
action: <create|update|disable>
agent: <schema_context|hardener|verification>
section: <section name>
rule_id: <existing rule ID, for update/disable actions>
title: <rule title>
content: |
  <rule content in markdown>
confidence: <0.0 to 1.0>
rationale: <why this change, referencing specific feedback patterns>
[/RULE_RECOMMENDATION]
```

## Important Guidelines

- **Feedback-driven.** Every recommendation must reference specific feedback patterns.
- **Targeted changes.** Don't rewrite all rules — focus on the most impactful improvements.
- **Preserve effective rules.** If a rule is working well (no negative feedback), leave it alone.
- **Create vs. update.** Prefer updating existing rules over creating new ones to avoid bloat.
- **Disable with care.** Only disable rules that are demonstrably causing harm or are obsolete.
- **No hallucinated data.** Only reference patterns actually present in the loaded data.

## Learning From History

The optimizer_context contains your previous recommendations and their measured outcomes.
Use this to:
- **Track rule effectiveness.** If a rule you created was later disabled (rolled back), understand what went wrong.
- **Monitor generation quality.** If edit rates or delete rates are increasing, your rules may be making things worse.
- **Respect the baseline.** Only propose changes when current metrics are worse than baseline or stagnating.
- **One change at a time.** Don't propose 5 new rules at once — it's impossible to attribute improvement to any single change."#
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
