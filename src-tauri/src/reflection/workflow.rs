//! Programmatic reflection workflow definition.
//!
//! Builds the execution steps for a reflection workflow, including:
//! - Setup phase: Load source run data via API requests
//! - Agentic phase: AI analyzes the data and applies fixes
//! - Verification phase: Verify no destructive changes
//! - Completion phase: Record patterns and evaluate effectiveness

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for a reflection workflow.
pub fn build_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 2,
        base_prompt: build_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true, // Run analysis before verification
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
    }
}

/// Build setup steps that load source run data via API requests.
pub fn build_setup_steps(source_task_run_id: &str, source_workflow_name: &str) -> Vec<ExecutionStepConfig> {
    let base_url = "http://localhost:9876";

    vec![
        // Step 1: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
        ),
        // Step 2: Load knowledge from source run
        build_api_step(
            "Load source knowledge",
            "GET",
            &format!("{}/task-runs/{}/knowledge", base_url, source_task_run_id),
            None,
            Some("source_knowledge"),
        ),
        // Step 3: Load AI conversation output (critical for holistic analysis)
        build_api_step(
            "Load AI output",
            "GET",
            &format!(
                "{}/task-runs/{}/output?tail_chars=30000",
                base_url, source_task_run_id
            ),
            None,
            Some("source_ai_output"),
        ),
        // Step 4: Load workflow execution state
        build_api_step(
            "Load workflow state",
            "GET",
            &format!(
                "{}/task-runs/{}/workflow-state",
                base_url, source_task_run_id
            ),
            None,
            Some("source_workflow_state"),
        ),
        // Step 5: Load previous reflection fixes for effectiveness evaluation
        build_api_step(
            "Load previous fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("previous_fixes"),
        ),
    ]
}

/// Build verification steps for the reflection workflow.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    // Reflection verification is lightweight — just ensure no destructive changes
    vec![build_prompt_step(
        "Verify reflection safety",
        r#"Verify that the reflection analysis is complete and no destructive changes were applied.

Check:
1. All significant findings from the source run were analyzed
2. No files were deleted or overwritten without recording the change
3. All applied fixes have been recorded via POST /reflection-fixes
4. Fix descriptions are clear and actionable

If any issues are found, report them. Otherwise, confirm the reflection is safe."#,
    )]
}

/// Build completion steps for the reflection workflow.
pub fn build_completion_steps(source_workflow_name: &str) -> Vec<ExecutionStepConfig> {
    vec![build_prompt_step(
        "Record patterns and evaluate",
        &format!(
            r#"Complete the reflection by:

1. Record any recurring patterns as knowledge entries using the standard [KNOWLEDGE:recurring_pattern] markers
2. Evaluate previous fixes by calling POST /reflection/evaluate?workflow_name={}
3. Generate a brief summary of:
   - Issues identified (count by category)
   - Fixes applied (count by type)
   - Previous fix effectiveness results
   - Recommendations for the next run"#,
            urlencoding::encode(source_workflow_name)
        ),
    )]
}

/// Build the agentic phase prompt for the reflection AI.
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a reflection agent analyzing the completed workflow run for "{}".

Your goal is to identify systemic issues and apply fixes that will improve subsequent runs.

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_findings}}}}` — Categorized findings with signature hashes
- `{{{{source_knowledge}}}}` — Observations, solutions, root causes recorded during execution
- `{{{{source_ai_output}}}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state (phases, iterations, timing)
- `{{{{previous_fixes}}}}` — Previous reflection fixes for this workflow (for effectiveness comparison)

## Analysis Steps

### Step 1: Holistic Output Analysis (PRIMARY)
Read the complete AI conversation output end-to-end and identify behavioral patterns NOT captured by inline findings:
- **Repeated failed attempts** (e.g., "tried 4 selectors before finding the right one")
- **Goal misunderstanding** (e.g., "spent 2 iterations on wrong approach before correcting")
- **Tool usage struggles** (e.g., "UI Bridge calls consistently failed with timeout")
- **Missing context indicators** (e.g., "AI asked 'where is X?' or searched extensively")
- **Wasted iterations** (e.g., "iteration 3 repeated work already done in iteration 1")
- **Decision quality** (e.g., "AI chose approach A but approach B would have been better")

### Step 2: Cross-reference with Structured Data
- Compare holistic observations against the localized findings and knowledge
- Identify gaps — issues the AI struggled with but didn't emit a [FINDING:...] for
- Note verification feedback patterns — what checks kept failing and why

### Step 3: Categorize All Issues
- `tool_failure` — tool/API/selector problems
- `context_gap` — AI lacked information it should have had
- `instruction_ambiguity` — workflow steps were unclear or misleading
- `recurring_pattern` — same issue seen across iterations or across runs

### Step 4: Apply Fixes and Record
For each actionable issue, apply the fix and record it via:
```
POST http://localhost:9876/reflection-fixes
Content-Type: application/json

{{
  "source_task_run_id": "<from source run>",
  "reflection_task_run_id": "<this run's ID>",
  "source_finding_id": "<optional, if linked to a specific finding>",
  "fix_type": "<knowledge_base_update|workflow_step_rewrite|selector_fix|tool_config_update|context_addition|instruction_clarification>",
  "fix_description": "<what was changed and why>",
  "file_changed": "<optional path>",
  "old_value": "<optional previous value>",
  "new_value": "<optional new value>",
  "confidence": "<high|medium|low>"
}}
```

**Confidence guidelines:**
- `high` — Clear root cause, straightforward fix, apply automatically
- `medium` — Likely fix but needs validation, apply and monitor
- `low` — Speculative, log but don't auto-apply

### Step 5: Evaluate Previous Fixes
Compare the source run's findings against previously applied fixes:
- If a fix's source finding signature does NOT appear in this run → fix was effective
- If the signature DOES appear → fix was ineffective
- If new findings appeared that weren't present before → possible regression

Record evaluations via:
```
PUT http://localhost:9876/reflection-fixes/<fix-id>/effectiveness
Content-Type: application/json

{{
  "effectiveness": "<effective|ineffective|caused_regression|inconclusive>",
  "effectiveness_evidence": "<explanation>"
}}
```"#,
        source_workflow_name
    )
}

/// Helper: Build an API request step.
fn build_api_step(
    name: &str,
    method: &str,
    url: &str,
    body: Option<&str>,
    output_variable: Option<&str>,
) -> ExecutionStepConfig {
    ExecutionStepConfig {
        step_type: "api_request".to_string(),
        name: Some(name.to_string()),
        api_method: Some(method.to_string()),
        api_url: Some(url.to_string()),
        api_body: body.map(|b| b.to_string()),
        api_output_variable: output_variable.map(|v| v.to_string()),
        is_setup: Some(true),
        run_on_subsequent_iterations: Some(false),
        ..Default::default()
    }
}

/// Helper: Build a prompt step.
fn build_prompt_step(name: &str, content: &str) -> ExecutionStepConfig {
    ExecutionStepConfig {
        step_type: "prompt".to_string(),
        name: Some(name.to_string()),
        prompt_content: Some(content.to_string()),
        ..Default::default()
    }
}
