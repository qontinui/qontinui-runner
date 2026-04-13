//! Pipeline Prompt Optimizer (Agent 1)
//!
//! Analyzes per-agent traces and reflection fixes to recommend improved
//! system prompts for pipeline agents (spec_analyst, locator, implementer, verifier).
//!
//! Follows the fixer/workflow.rs pattern.

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Thinking style variations for mutation diversity (PromptWizard-inspired).
/// Rotated across optimizer runs to explore different optimization angles.
const PIPELINE_THINKING_STYLES: &[&str] = &[
    // Style 0: Conservative
    "**Conservative approach:** Make minimal, surgical changes. Preserve all working patterns. \
     Focus exclusively on the single highest-impact failure pattern. Prefer tightening existing \
     instructions over adding new ones.",
    // Style 1: Structural
    "**Structural approach:** Consider reorganizing the prompt structure. Evaluate whether \
     reordering sections, adding explicit role definitions, or restructuring the task decomposition \
     would help. Focus on clarity of prompt architecture over individual instructions.",
    // Style 2: Example-driven
    "**Example-driven approach:** Focus on adding concrete examples and counter-examples to the \
     prompt. Show the agent exactly what good and bad outputs look like. Use before/after pairs \
     that demonstrate the desired behavior change.",
    // Style 3: Persona-based
    "**Persona-based approach:** Assign a domain expert persona to the agent and frame instructions \
     as what that expert would prioritize. Consider what mental model or checklist an expert in the \
     relevant domain would follow.",
];

/// Build the LoopConfig for the pipeline prompt optimizer.
pub fn build_config(execution_id: &str, workflow_name: &str, style_index: u32) -> LoopConfig {
    let base_url = crate::mcp::types::get_self_base_url_from_env();
    let style = PIPELINE_THINKING_STYLES[(style_index as usize) % PIPELINE_THINKING_STYLES.len()];
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_agentic_prompt(&base_url, style),
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
        blocking_approval: false,
        confidence_threshold: 0.85,
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
        max_fix_attempts: 3,
        max_ci_auto_resumes: 10,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
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
            &format!(
                "{}/meta-optimizer/optimizer-context?type=pipeline_prompt",
                base_url
            ),
            None,
            Some("optimizer_context"),
        ),
        // Step 1: Load per-agent trace summaries (L0 — agent_type + count + success_pct)
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
        // Step 2: Load reflection fix summaries (L0 — counts by fix_type and effectiveness)
        build_api_step(
            "Load reflection fix summaries (L0)",
            "GET",
            &format!(
                "{}/meta-optimizer/reflection-fixes?limit=100&tier=l0",
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
        // Step 5: Load cost efficiency analysis
        build_api_step(
            "Load cost efficiency analysis",
            "GET",
            &format!("{}/meta-optimizer/cost-analysis", base_url),
            None,
            Some("cost_analysis"),
        ),
        // Step 6: Load recent workflow outcomes for batch scoring (L1 — core fields without large blobs)
        build_api_step(
            "Load recent outcomes for batch scoring (L1)",
            "GET",
            &format!("{}/learning/outcomes?limit=20&tier=l1", base_url),
            None,
            Some("recent_outcomes"),
        ),
        // Step 7: Load iteration history patterns (L0 — approach success/failure patterns)
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

fn build_agentic_prompt(base_url: &str, thinking_style: &str) -> String {
    let prompt = r#"You are the Pipeline Prompt Optimizer, part of the meta-optimizer system.

Your job is to analyze historical performance data from ALL workflow agents and recommend improved system prompts for underperforming agents.

## Agent Types

Traces come from two workflow architectures:

**Multi-Agent Pipeline agents:**
- `spec_analyst` — Analyzes specifications and requirements
- `locator` — Finds relevant code locations
- `implementer` — Makes code changes
- `verifier` — Verifies changes are correct

**Traditional (Agentic Verification) loop agents:**
- `verification` — Runs verification checks in the agentic loop
- `agentic_fixer` — Applies fixes in the agentic loop

You can recommend prompt changes for ANY of these agent types. The same recommendation format works for all of them.

You will follow a **Generate → Critique → Refine** loop to produce high-quality recommendations.

## Data Available (Tiered Context — L0 Summaries)

The setup phase loaded **L0 summaries** (lightweight index data) to minimize token usage:
- `{{optimizer_context}}` — Your performance history: previous recommendations and their outcomes, current metrics vs baseline, top failure patterns, per-agent failure rates
- `{{agent_trace_aggregates}}` — **L0 summary**: agent_type, run_count, success_pct (one row per agent type)
- `{{reflection_fixes}}` — **L0 summary**: fix_type, total_count, effective_count (grouped by fix type)
- `{{prompt_variants}}` — Current active prompt variants in the registry (may be empty if using defaults)
- `{{prompt_analysis}}` — Historical prompt analysis insights
- `{{cost_analysis}}` — Cost efficiency analysis: per-agent cost breakdown, total cost, cost trend, active cost recommendations
- `{{recent_outcomes}}` — **L1 detail**: Last 20 workflow outcomes (id, task_id, status, duration, iterations, architecture, error_type, created_at) for batch scoring
- `{{iteration_history}}` — **L0 summary**: approach patterns from agentic verification loops (avg iterations, most common approaches, confidence trends by run status)

## Drill-Down: L1/L2 Detail Endpoints

If the L0 summaries reveal an agent with low success or a fix type with high count, you can drill into details using curl:

**Agent Traces — L1 (core fields per trace):**
```bash
curl -s '{{base_url}}/meta-optimizer/agent-trace-aggregates?tier=l1&agent_type=<AGENT_TYPE>&limit=20'
```
Returns: id, agent_type, duration_ms, downstream_success, created_at per trace.

**Agent Traces — L2 (full aggregated statistics):**
```bash
curl -s '{{base_url}}/meta-optimizer/agent-trace-aggregates?tier=l2&limit=50'
```
Returns: full aggregates with avg_duration_ms, avg_cost_usd, avg_tokens_in/out.

**Reflection Fixes — L1 (core fields per fix):**
```bash
curl -s '{{base_url}}/meta-optimizer/reflection-fixes?tier=l1&limit=50'
```
Returns: id, fix_type, fix_description, confidence, effectiveness, source_agent, created_at.

**Reflection Fixes — L2 (full records):**
```bash
curl -s '{{base_url}}/meta-optimizer/reflection-fixes?tier=l2&limit=50'
```
Returns: full records including reasoning, old_value, new_value.

Only drill into L1/L2 when the L0 summaries show a problem worth investigating. Do NOT load L2 data for all agents — only for the 1-2 agents that need prompt changes.

## Prerequisites — Check Before Proceeding

Before generating any recommendations, verify using the L0 summaries:
1. **Agent trace data exists.** Check {{agent_trace_aggregates}} for non-empty data. If no traces exist for an agent type, you CANNOT recommend prompt changes for it — you have no evidence of what's failing.
2. **Sufficient sample size.** At least 10 runs per agent type are needed for meaningful analysis (check run_count in L0). If an agent has fewer than 10 traces, note this limitation and lower your confidence accordingly.
3. **Clear failure patterns.** Reflection fix summaries ({{reflection_fixes}}) should show fix types with high counts attributable to a specific agent. Drill into L1 to confirm patterns before recommending changes.

If these prerequisites are NOT met, produce a brief analysis explaining what data is missing and output ZERO [PROMPT_RECOMMENDATION] markers. This is the correct behavior — recommending changes without data is worse than recommending nothing.

## Your Task: Generate → Critique → Refine

### Phase 1: GENERATE — Draft Candidate Prompts

#### Step 1: Triage with L0 Summaries

Review the L0 summaries to identify which agents need investigation:
- Which agents have low success_pct (< 70%)?
- Which fix types have the highest counts?
- Use L1 drill-down on the 1-2 worst-performing agents to understand failure patterns.

#### Step 2: Identify Underperformers

Focus on agents with:
- Low downstream success rates (< 70% in L0 success_pct)
- High cost relative to output quality (drill into L2 for cost data if needed)
- Recurring reflection fix patterns indicating prompt weakness (use L1 for fix details)
- Patterns where the agent misunderstands its role or produces poor-quality output

#### Step 3: Draft 2-3 Candidate Prompt Variants

For each underperforming agent, draft 2-3 distinct candidate prompt variants. Each candidate should take a different approach to addressing the failure patterns. Output each as:

```
[PROMPT_CANDIDATE]
candidate_id: <A, B, or C>
agent_type: <spec_analyst|locator|implementer|verifier|verification|agentic_fixer>
approach: <brief description of the approach taken>
prompt_content: |
  <the full new prompt text>
[/PROMPT_CANDIDATE]
```

Ensure diversity between candidates — don't produce minor variations of the same idea. Each should take a meaningfully different approach (e.g., one structural, one example-driven, one that tightens constraints).

### Phase 2: CRITIQUE — Self-Evaluate Each Candidate

For each candidate, evaluate against these criteria and output your analysis:

```
[PROMPT_CRITIQUE]
candidate_id: <matching candidate_id>
failure_patterns_addressed: <which specific failure patterns from the data does this candidate fix?>
failure_patterns_missed: <which failure patterns does this candidate NOT address?>
regression_risk: <what currently-working patterns might this candidate break?>
duplicate_check: <does this duplicate or closely resemble a previously-rejected recommendation from {{optimizer_context}}?>
batch_score: <review the {{recent_outcomes}} — estimate how many of the recent failures this candidate would have prevented (0-20)>
verdict: <advance|discard>
verdict_reason: <why>
[/PROMPT_CRITIQUE]
```

**Discard a candidate if:**
- It duplicates a previously-rejected recommendation
- It would prevent fewer than 3 of the 20 recent failures (batch score < 3)
- Its regression risk outweighs its expected improvement
- It doesn't address any specific failure pattern from the data

### Phase 3: REFINE — Produce Final Recommendations

From the surviving candidates (verdict=advance), produce the final refined recommendations. Incorporate critique findings — if a candidate was advanced but had weaknesses noted in the critique, address those weaknesses in the final version.

For each recommendation, output:

```
[PROMPT_RECOMMENDATION]
agent_type: <spec_analyst|locator|implementer|verifier|verification|agentic_fixer>
variant_name: <descriptive name, e.g. "clarity_focused_v2">
confidence: <0.0 to 1.0>
rationale: <why this change should improve performance, referencing specific failure patterns and batch score>
prompt_content: |
  <the full new prompt text>
[/PROMPT_RECOMMENDATION]
```

## Quality Gate

- **Minimum confidence: 45%.** Recommendations between 45-60% confidence are saved for human review. Recommendations above 60% may be auto-applied if above 85%. This lower threshold allows the system to capture weak-signal improvements that still have merit.
- **Must cite specific data.** Every recommendation must reference specific numbers from agent_trace_aggregates or reflection_fixes. "The failure rate is high" is not sufficient — "implementer has 22% downstream failure rate across 45 runs, with 8 reflection fixes citing 'incorrect file targeting'" is.
- **No data = no recommendation.** If agent_trace_aggregates is empty or has zero entries, output zero recommendations. Write a brief explanation of what data is needed instead.
- **Maximum 2 recommendations per run.** Focus on the highest-impact changes. More than 2 prompt changes at once makes it impossible to measure which one helped.
- **Batch scoring required.** Every recommendation must include its estimated batch score from the critique phase. Recommendations with batch_score < 3 should not be output.

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

- **Evidence-based changes only.** Every recommendation must reference specific data from the traces or reflection fixes.
- **Only change what's clearly broken.** If an agent has >80% downstream success rate, do NOT recommend changes. If you have fewer than 10 traces for an agent, do NOT recommend changes. Say explicitly: "Agent X has Y% success across Z runs — no changes recommended."
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
- **Don't recommend what's already applied.** Check the active prompt variants before suggesting new ones."#;
    prompt
        .replace("{{base_url}}", base_url)
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
