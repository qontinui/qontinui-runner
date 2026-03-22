//! Generation Template Optimizer (Agent 3)
//!
//! Improves workflow generation rules used by builder, verification, and
//! hardener agents in generator.rs.
//!
//! Follows the fixer/workflow.rs pattern.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Thinking style variations for mutation diversity (PromptWizard-inspired).
/// Rotated across optimizer runs to explore different optimization angles.
const GENERATION_THINKING_STYLES: &[&str] = &[
    // Style 0: Conservative
    "**Conservative approach:** Make minimal targeted rule changes. Focus on the single most \
     impactful rule improvement. Prefer tightening existing rules over creating new ones.",
    // Style 1: Consolidation
    "**Consolidation approach:** Look for rules that can be merged or simplified. Identify \
     redundant or overlapping rules that cause confusion. Reduce rule count while preserving \
     coverage.",
    // Style 2: Example-enrichment
    "**Example-enrichment approach:** Focus on adding positive/negative examples to existing rules \
     that lack them. Concrete examples are more effective than abstract instructions. Prioritize \
     rules with high failure_count that have no examples.",
    // Style 3: Gap analysis
    "**Gap analysis approach:** Look for failure patterns in {{recent_outcomes}} that have no \
     corresponding rule. Find the blind spots in the current rule set. Create targeted rules for \
     uncovered failure modes.",
];

/// Build the LoopConfig for the generation template optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str, style_index: u32) -> LoopConfig {
    let style =
        GENERATION_THINKING_STYLES[(style_index as usize) % GENERATION_THINKING_STYLES.len()];
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_agentic_prompt(style),
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
        enforce_token_budget: false,
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
        rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
        iteration_diffs: Vec::new(),
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
            &format!(
                "{}/meta-optimizer/optimizer-context?type=generation_template",
                base_url
            ),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load workflow generation feedback (edits, deletes, ratings)
        build_api_step(
            "Load generation feedback",
            "GET",
            &format!("{}/workflow-generation/feedback?limit=100", base_url),
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

fn build_agentic_prompt(thinking_style: &str) -> String {
    r#"You are the Generation Template Optimizer, part of the meta-optimizer system.

Your job is to analyze workflow generation feedback and recommend improvements to the generation rules used by the builder, verification, and hardener agents.

You will follow a **Generate → Critique → Refine** loop to produce high-quality recommendations.

## Data Available

The setup phase loaded:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, generation quality trends
- `{{generation_feedback}}` — User feedback on generated workflows: edits, deletes, ratings, and which fields were commonly edited
- `{{generation_rules}}` — Current active generation rules for all agents (schema_context, hardener, verification). Rules may include an `examples` field with positive/negative examples.
- `{{prompt_analysis}}` — Historical prompt analysis insights
- `{{recent_outcomes}}` — Recent task run outcomes (success/failure patterns) — use for batch scoring during critique

## Generation Rules System

Rules are organized by:
- **agent**: `schema_context`, `hardener`, `verification`
- **section**: `important_rules`, `verification_quality`, `conversion_rules`, etc.
- **provenance**: `seed` (original), `reflection` (from reflection fixes), `auto_insight` (auto-generated)
- **status**: `active`, `disabled`, `superseded`

Each rule has instruction text in `content` and may optionally have `examples` (positive/negative pairs). Rules without examples are less effective — the system learns better from concrete demonstrations.

## Your Task: Generate → Critique → Refine

### Phase 1: GENERATE — Draft Candidate Rule Changes

#### Step 1: Analyze Feedback Patterns

Look for:
- Which fields are users editing most frequently? (indicates generation is getting them wrong)
- Which generated workflows are being deleted? (indicates fundamental quality issues)
- What ratings are users giving? Are there patterns in low-rated workflows?
- Are there common edit patterns that could be captured as rules?

#### Step 2: Evaluate Current Rules

For each active rule:
- Is it still relevant? (evidence in recent outcomes)
- Is it being followed? (check against recent feedback)
- Could it be improved or made more specific?
- Are there any conflicting rules?
- Does it have examples? If not, it's a candidate for example enrichment.

#### Step 3: Draft 2-3 Candidate Rule Changes

For each proposed change, output a candidate marker:

```
[RULE_CANDIDATE]
candidate_id: <A, B, or C>
action: <create|update|disable>
agent: <schema_context|hardener|verification>
section: <section name>
rule_id: <existing rule ID, for update/disable actions>
title: <rule title>
content: |
  <rule content in markdown>
[/RULE_CANDIDATE]
```

Ensure diversity between candidates — don't produce minor variations of the same rule. Each should address a different problem or take a different approach.

### Phase 2: CRITIQUE — Self-Evaluate Each Candidate

For each candidate, evaluate and output:

```
[RULE_CRITIQUE]
candidate_id: <matching candidate_id>
feedback_patterns_addressed: <which specific feedback patterns does this candidate fix?>
feedback_patterns_missed: <what patterns does this candidate NOT address?>
conflict_check: <does this conflict with any existing active rule in {{generation_rules}}?>
duplicate_check: <does this duplicate a previously-rejected recommendation from {{optimizer_context}}?>
batch_score: <review {{recent_outcomes}} — estimate how many of the recent failures this rule would have prevented (0-20)>
verdict: <advance|discard>
verdict_reason: <why>
[/RULE_CRITIQUE]
```

**Discard a candidate if:**
- It duplicates a previously-rejected recommendation
- It conflicts with an existing effective rule
- It would prevent fewer than 2 of the recent failures (batch score < 2)
- It doesn't address any specific feedback pattern from the data

### Phase 3: REFINE — Produce Final Recommendations

From surviving candidates (verdict=advance), produce final refined recommendations incorporating critique findings:

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
rationale: <why this change, referencing specific feedback patterns and batch score>
[/RULE_RECOMMENDATION]
```

### Phase 4 (Optional): GENERATE EXAMPLES for Existing Rules

For any existing rule in {{generation_rules}} that has **no examples**, you may generate synthetic positive/negative examples to strengthen it. This is especially valuable for high-severity rules.

```
[RULE_EXAMPLE]
rule_id: <existing rule ID from {{generation_rules}}>
positive_example: |
  <a concrete workflow snippet that follows this rule correctly>
negative_example: |
  <a concrete workflow snippet that violates this rule>
positive_explanation: <why the positive example is correct>
negative_explanation: <why the negative example is wrong and what would break>
[/RULE_EXAMPLE]
```

Focus on rules that are frequently violated (high failure_count) or critical severity. Generate at most 5 [RULE_EXAMPLE] blocks per run.

## Quality Gate

- **Minimum confidence: 45%.** Recommendations between 45-60% confidence are saved for human review. Recommendations above 60% may be auto-applied if above 85%. This lower threshold allows the system to capture weak-signal improvements.
- **Must cite specific evidence.** Every rule recommendation must reference a specific pattern from generation_feedback, a specific existing rule being improved, or a specific failure pattern from recent_outcomes.
- **Maximum 3 recommendations per run.** Focus on the highest-impact changes. Creating many rules at once causes bloat and makes attribution impossible.
- **Check for existing rules first.** Before proposing a new rule, verify it doesn't duplicate an existing active rule from {{generation_rules}}. If a similar rule exists, propose an update instead of a create.
- **Validate agent names.** Only use these agent names: `schema_context`, `hardener`, `verification`. Any other agent name will cause the rule to be ignored.
- **Validate section names.** Common sections: `important_rules`, `verification_quality`, `conversion_rules`, `ui_bridge_rules`. Check {{generation_rules}} for actual section names in use.
- **Batch scoring required.** Every recommendation must include its estimated batch score from the critique phase. Recommendations with batch_score < 2 should not be output.

## Expert Persona

When analyzing failures and generating improvements, adopt the perspective matching the dominant failure category:
- A **DevOps reliability engineer** for infrastructure/deployment-related failures
- A **QA test architect** for verification/testing-related failures
- An **API integration specialist** for SDK/endpoint-related failures
- A **UX engineer** for UI Bridge-related failures

Match your persona to the dominant failure category in the data. Frame recommendations as this expert would.

## Optimization Approach for This Run

{{thinking_style}}

Use this approach to guide your candidate generation in Phase 1. This style is intentionally varied across optimizer runs to explore different optimization directions and prevent convergence to local optima.

## Important Guidelines

- **Feedback-driven.** Every recommendation must reference specific feedback patterns.
- **Targeted changes.** Don't rewrite all rules — focus on the most impactful improvements.
- **Preserve effective rules.** If a rule is working well (no negative feedback), leave it alone.
- **Create vs. update.** Prefer updating existing rules over creating new ones to avoid bloat.
- **Disable with care.** Only disable rules that are demonstrably causing harm or are obsolete.
- **No hallucinated data.** Only reference patterns actually present in the loaded data.
- **No speculative rules.** Don't create rules for problems you think might exist. Only create rules for problems you can see in the data.
- **Rules must be specific and testable.** "Write better code" is not a rule. "When a curl command pipes output to python, do not use -f flag because it suppresses error output needed by the parser" is a rule.

## Learning From History

The optimizer_context contains your previous recommendations and their measured outcomes.
Use this to:
- **Track rule effectiveness.** If a rule you created was later disabled (rolled back), understand what went wrong.
- **Monitor generation quality.** If edit rates or delete rates are increasing, your rules may be making things worse.
- **Respect the baseline.** Only propose changes when current metrics are worse than baseline or stagnating.
- **One change at a time.** Don't propose 5 new rules at once — it's impossible to attribute improvement to any single change."#
        .replace("{{thinking_style}}", thinking_style)
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
