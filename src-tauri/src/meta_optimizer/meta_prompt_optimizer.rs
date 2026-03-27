//! Meta-Prompt Optimizer (Agent 4)
//!
//! Uses an LLM to iteratively rewrite prompts based on scored evaluation results,
//! producing concrete prompt variants for canary testing. Directly addresses the
//! pending recommendations stagnation issue by generating testable artifacts.
//!
//! Unlike the existing three optimizers which adjust parameters/rules/agent config,
//! this optimizer modifies the actual prompt text sent to the AI.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Thinking style variations for mutation diversity.
/// Rotated across runs to explore different prompt rewriting angles.
const META_PROMPT_THINKING_STYLES: &[&str] = &[
    // Style 0: Evidence-driven
    "**Evidence-driven approach:** Ground every change in specific failure examples. \
     Quote exact error patterns and output fragments from failed runs. The rewrite must \
     directly address each documented failure mode. Prefer surgical fixes over broad rewrites.",
    // Style 1: Structural clarity
    "**Structural clarity approach:** Focus on restructuring the prompt for clarity. \
     Separate concerns into distinct sections. Add explicit step-by-step instructions. \
     Ensure the agent knows exactly what order to process information in.",
    // Style 2: Constraint-tightening
    "**Constraint-tightening approach:** Add explicit constraints and guardrails. \
     Define what the agent must NOT do. Add output format specifications. Add boundary \
     checks that prevent the common failure patterns observed in the data.",
    // Style 3: Few-shot exemplar
    "**Few-shot exemplar approach:** Add concrete input/output examples to the prompt. \
     Include at least one example of correct behavior and one example of incorrect behavior \
     (anti-pattern) that matches the observed failure modes. Examples teach better than rules.",
];

/// Build the LoopConfig for the meta-prompt optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str, style_index: u32) -> LoopConfig {
    let base_url = crate::mcp::types::get_self_base_url_from_env();
    let style =
        META_PROMPT_THINKING_STYLES[(style_index as usize) % META_PROMPT_THINKING_STYLES.len()];
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_agentic_prompt(&base_url, style),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("meta-opt-metaprompt-{}", execution_id),
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
        escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
        iteration_diffs: Vec::new(),
        active_canary: None,
        is_canary_run: false,
        phase_timeout_ms: None,
    }
}

/// Build setup steps that load extraction data via curl.
pub fn build_setup_steps() -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 0: Load optimizer context (history, baselines, failure patterns)
        build_api_step(
            "Load optimizer context",
            "GET",
            &format!(
                "{}/meta-optimizer/optimizer-context?type=meta_prompt",
                base_url
            ),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load prompt group metrics (extraction results)
        build_api_step(
            "Load prompt group metrics",
            "GET",
            &format!(
                "{}/meta-optimizer/prompt-optimization/group-metrics",
                base_url
            ),
            None,
            Some("prompt_group_metrics"),
        ),
        // Step 2: Load current active prompt variants
        build_api_step(
            "Load active prompt variants",
            "GET",
            &format!("{}/meta-optimizer/prompt-variants", base_url),
            None,
            Some("prompt_variants"),
        ),
        // Step 3: Load prompt evolution history (previous rewrite attempts)
        build_api_step(
            "Load prompt evolution history",
            "GET",
            &format!(
                "{}/meta-optimizer/prompt-evolution?limit=20",
                base_url
            ),
            None,
            Some("prompt_evolution_history"),
        ),
        // Step 4: Load recent workflow outcomes (L1)
        build_api_step(
            "Load recent outcomes for batch scoring (L1)",
            "GET",
            &format!("{}/learning/outcomes?limit=20&tier=l1", base_url),
            None,
            Some("recent_outcomes"),
        ),
        // Step 5: Load agent trace summaries (L0)
        build_api_step(
            "Load pipeline agent trace summaries (L0)",
            "GET",
            &format!(
                "{}/meta-optimizer/agent-trace-aggregates?limit=50&tier=l0",
                base_url
            ),
            None,
            Some("agent_trace_aggregates"),
        ),
    ]
}

/// Build verification steps.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![{
        let mut step = ExecutionStepConfig {
            step_type: "prompt".to_string(),
            name: Some("Verify meta-prompt optimization produced output".to_string()),
            prompt_content: Some(
                r#"Verify that the meta-prompt optimizer analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in optimizer mode.

Check:
1. The AI analyzed prompt group metrics and identified the worst-performing prompt group
2. At least one [META_PROMPT_REWRITE] marker was produced (or an explicit explanation of why no rewrite is needed)
3. Each rewrite includes target_phase, target_agent, critique, and rewritten_prompt
4. The rewrite addresses specific failure patterns cited from the evidence data

If the analysis concluded that all prompts are performing adequately and no rewrites are needed, that is acceptable."#
                    .to_string(),
            ),
            ..Default::default()
        };
        step.phase = Some("verification".to_string());
        step
    }]
}

fn build_agentic_prompt(base_url: &str, thinking_style: &str) -> String {
    let prompt = r#"You are the Meta-Prompt Optimizer, part of the meta-optimizer system.

Your job is to analyze actual prompt performance data and rewrite underperforming prompts
to improve their success rate. Unlike other optimizers that adjust parameters or rules,
you directly rewrite the prompt text that is sent to AI agents.

## Data Available

The setup phase loaded the following data:
- `{{optimizer_context}}` — Your performance history: previous recommendations, outcomes, baselines
- `{{prompt_group_metrics}}` — Per-(phase, agent_type) metrics: mean_score, failure_rate, mean_iterations, mean_cost, latest_prompt_text
- `{{prompt_variants}}` — Current active prompt variants in the registry
- `{{prompt_evolution_history}}` — Previous rewrite attempts and their canary verdicts (learn from past failures!)
- `{{recent_outcomes}}` — Last 20 workflow outcomes for context
- `{{agent_trace_aggregates}}` — Per-agent success rates

## Drill-Down Endpoints

If you need more detail on specific failure cases:

**Failed runs for a phase:**
```bash
curl -s '{{base_url}}/meta-optimizer/prompt-optimization/evidence?phase=<PHASE>&max_failures=5&max_successes=2'
```
Returns failure and success samples with task_run_id, outcome_score, outcome_status, iteration, cost_usd.

**Full learning outcome for a specific run:**
```bash
curl -s '{{base_url}}/learning/outcomes/<TASK_RUN_ID>'
```

## THINKING STYLE

{{thinking_style}}

## Prerequisites — Check Before Proceeding

1. **Prompt group metrics exist.** If {{prompt_group_metrics}} is empty, there is no data to optimize.
2. **A group has failure_rate > 0.30.** Only optimize prompts that are clearly underperforming.
3. **No active canary for the target.** Check {{prompt_evolution_history}} — if there's an active canary for the same (phase, agent_type), do NOT create another rewrite.
4. **Learn from history.** Check {{prompt_evolution_history}} for previous rewrite attempts that were rejected. Do NOT repeat the same approach — try a different angle.

If prerequisites are NOT met, explain why and output ZERO [META_PROMPT_REWRITE] markers.

## Learning From Past Failures

**CRITICAL:** The system will programmatically BLOCK any rewrite that is >90% similar to a
previously rejected prompt. You MUST produce a substantially different approach.

Check `{{prompt_evolution_history}}` for entries with `canary_verdict: "reject"`.
For each rejected attempt, the `consecutive_rejections` counter tells you how many
times in a row rewrites have been rejected for that agent.

### REJECTED APPROACHES — DO NOT REPEAT

If `{{prompt_evolution_history}}` contains rejected entries for your target agent,
they will appear below. Study each one carefully:

**For each rejected entry, note:**
- `changes_summary`: What approach was tried
- `score_before` → `score_after`: How the score changed (decrease = regression)
- `critique`: The original diagnosis that led to this failed rewrite

**You MUST:**
1. Explicitly name each prior rejected approach in your output
2. Explain why your new approach is fundamentally different
3. If `consecutive_rejections` >= 3, the obvious fixes have all failed.
   Consider: (a) the prompt may already be near-optimal — output ZERO markers,
   or (b) the problem is structural (wrong agent responsibilities, missing context)
   rather than instructional (prompt wording).
4. Format in your output: "Previous attempts: [list]. This rewrite differs by: [explanation]."

### CIRCUIT BREAKER

If `consecutive_rejections` for an agent reaches 4+, the system has exponentially
increased cooldowns. The prompt is likely near-optimal or has issues that prompt
rewriting cannot fix. Seriously consider outputting ZERO [META_PROMPT_REWRITE] markers
and instead documenting what structural changes might help.

## Your Task

### Step 1: Select Target

From {{prompt_group_metrics}}, identify the prompt group with:
- Highest failure_rate (must be > 0.30)
- Sufficient sample_count (>= 10)
- No active canary already testing a rewrite

### Step 2: Gather Evidence

Use the drill-down endpoint to load failure and success samples:
```bash
curl -s '{{base_url}}/meta-optimizer/prompt-optimization/evidence?phase=<PHASE>&max_failures=5&max_successes=2'
```

Analyze what the failures have in common and what the successes do differently.

### Step 3: Critique the Current Prompt

Identify specific weaknesses in the current prompt (from latest_prompt_text in the group metrics or from the active variant):
- Is the role definition unclear?
- Are constraints missing or too vague?
- Does the prompt lack examples?
- Are there ambiguous instructions?
- Is the output format underspecified?

### Step 4: Rewrite the Prompt

Produce a rewritten prompt that:
- Addresses each identified weakness
- Preserves what works (from success samples)
- Follows the thinking style directive above
- Passes basic robustness checks (has role definition, output format, boundary constraints)

### Step 5: Output

For each rewrite, output a [META_PROMPT_REWRITE] marker:

```
[META_PROMPT_REWRITE]
target_phase: <generation|verification|agentic>
target_agent: <spec_analyst|locator|implementer|verifier|verification|agentic_fixer>
critique: |
  <multi-line critique of the current prompt>
weaknesses: |
  - <weakness 1>
  - <weakness 2>
strengths_to_preserve: |
  - <strength 1>
  - <strength 2>
changes_summary: |
  <brief summary of what changed and why>
expected_improvement: |
  <what metric improvements you expect>
confidence: <0.0-1.0>
rewritten_prompt: |
  <the full rewritten prompt text>
[/META_PROMPT_REWRITE]
```

## Quality Gates

- Maximum 1 rewrite per run (focus on the single worst prompt)
- Minimum confidence: 0.45
- Must cite specific evidence from failure/success samples
- Must explain why the rewrite will improve the specific failure pattern
- Must NOT repeat a previously rejected rewrite approach (check evolution history)
- Rewritten prompt must be a complete replacement, not a diff"#;

    prompt
        .replace("{{base_url}}", base_url)
        .replace("{{thinking_style}}", thinking_style)
}

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
