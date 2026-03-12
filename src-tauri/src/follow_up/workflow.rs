//! Programmatic follow-up workflow definition.
//!
//! Builds the execution steps for a follow-up workflow, including:
//! - Setup phase: Load source run AI output and workflow state
//! - Agentic phase: AI reads output, identifies unfixed issues, fixes them
//! - Verification phase: Check that fix attempts were made

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for a follow-up workflow.
pub fn build_follow_up_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("follow-up-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true, // Run fix attempts before verification
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade follow-up
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        reflection_mode: false,
        provider_override: None,
        model_override: None,
        model_overrides: std::collections::HashMap::new(),
        stage_index: None,
        max_sessions: Some(5),
        auto_run_generated: false,
        approval_gate: false,
        max_context_tokens: 100_000,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
    }
}

/// Build setup steps that load source run data via curl commands.
pub fn build_setup_steps(source_task_run_id: &str) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 1: Load AI conversation output (primary data source)
        build_api_step(
            "Load source AI output",
            "GET",
            &format!(
                "{}/task-runs/{}/output?tail_chars=40000",
                base_url, source_task_run_id
            ),
            None,
            Some("source_ai_output"),
        ),
        // Step 2: Load workflow execution state
        build_api_step(
            "Load source workflow state",
            "GET",
            &format!(
                "{}/task-runs/{}/workflow-state",
                base_url, source_task_run_id
            ),
            None,
            Some("source_workflow_state"),
        ),
        // Step 3: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
        ),
    ]
}

/// Build verification steps for the follow-up workflow.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![
        // Lightweight prompt check that follow-up markers were produced
        {
            let mut step = ExecutionStepConfig {
                step_type: "prompt".to_string(),
                name: Some("Verify follow-up fix attempts".to_string()),
                prompt_content: Some(
                    r#"Verify that the follow-up analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in follow-up mode.
Base your verification ONLY on the data already loaded in runtime variables and your own output.

Check:
1. The AI identified unfixed issues from the source output
2. Each identified issue was either fixed ([FOLLOW_UP_FIX]) or explicitly skipped ([FOLLOW_UP_SKIP])
3. Fix attempts include clear descriptions of what was changed

If no unfixed issues were found in the source output, that is acceptable — confirm this.
Otherwise, verify that all identified issues have been addressed or explicitly skipped."#
                        .to_string(),
                ),
                ..Default::default()
            };
            step.phase = Some("verification".to_string());
            step
        },
    ]
}

/// Build the agentic phase prompt for the follow-up AI.
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a follow-up agent completing unfinished work from the workflow "{}".

Your goal is to identify issues that were NOT fixed during the source workflow run and fix them now.

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_ai_output}}}}` — The complete AI conversation output from the source run (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state (phases, iterations, timing)
- `{{{{source_findings}}}}` — Categorized findings from the source run

## Your Task

1. **Read the source AI output carefully end-to-end**
2. **Identify all unfixed issues** — look for:
   - Items listed under "Lower Priority", "Not Fixed", "Remaining Issues", "Deferred"
   - Items marked with `[unfixable_errors]` or similar
   - Issues the AI mentioned but explicitly skipped or deprioritized
   - Errors that persisted through all iterations without resolution
   - Items described as "out of scope" that are actually addressable
3. **For each unfixed issue, attempt to fix it** — you have full access to the codebase and tools
4. **Mark each issue** with one of these markers:

### Fix Markers

For each issue you successfully fix:
```
[FOLLOW_UP_FIX]
Issue: Brief description of the unfixed issue from the source run
Fix: What you did to fix it
Files: list of files modified
[/FOLLOW_UP_FIX]
```

For each issue you cannot fix or choose to skip:
```
[FOLLOW_UP_SKIP]
Issue: Brief description of the unfixed issue
Reason: Why it cannot be fixed (e.g., requires external dependency, needs human decision, etc.)
[/FOLLOW_UP_SKIP]
```

## Important Guidelines

- **DO fix actual code issues** — unlike reflection (which only analyzes), you should make real code changes
- **DO NOT repeat work already done** — the source workflow already completed its primary goals
- **Focus on the leftovers** — items explicitly noted as unfixed, deferred, or lower priority
- **Be practical** — if an issue truly requires human decision-making or external access, use [FOLLOW_UP_SKIP]
- **Keep changes minimal** — fix only the specific unfixed issues, don't refactor surrounding code
- **Test your fixes** — run relevant tests or verification after making changes

## Output Structure

Start your analysis with a summary:
```
## Unfixed Issues from Source Run
1. [issue description] — Status: WILL_FIX / WILL_SKIP
2. [issue description] — Status: WILL_FIX / WILL_SKIP
...
```

Then proceed to fix each WILL_FIX item, marking with [FOLLOW_UP_FIX] as you go.
Finally, mark any WILL_SKIP items with [FOLLOW_UP_SKIP].

End with a summary:
```
## Follow-Up Summary
- Issues identified: N
- Issues fixed: N
- Issues skipped: N
```"#,
        source_workflow_name
    )
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
            "curl -sf -X {} -H \"Content-Type: application/json\" -d '{}' '{}'",
            method, body_str, url
        )
    } else {
        format!("curl -sf -X {} '{}'", method, url)
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
