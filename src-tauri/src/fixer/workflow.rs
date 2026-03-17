//! Programmatic fixer workflow definition.
//!
//! Builds the execution steps for a fixer workflow, including:
//! - Setup phase: Load source run output, findings, reflection fixes, and child summaries
//! - Agentic phase: AI reads all outputs, identifies unfixed issues, implements fixes
//! - Verification phase: Check that fixer markers were produced

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for a fixer workflow.
pub fn build_fixer_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 5,
        base_prompt: build_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("fixer-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true,
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        constraint_overrides: std::collections::HashMap::new(),
        reflection_mode: true,
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
        acceptance_criteria: None,
    }
}

/// Build setup steps that load source run data and child outputs via curl commands.
pub fn build_setup_steps(source_task_run_id: &str) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 1: Load AI conversation output from source run
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
        // Step 2: Load findings from source run
        build_api_step(
            "Load source findings",
            "GET",
            &format!("{}/findings/task/{}", base_url, source_task_run_id),
            None,
            Some("source_findings"),
        ),
        // Step 3: Load reflection fixes for source run
        build_api_step(
            "Load reflection fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?source_task_run_id={}",
                base_url, source_task_run_id
            ),
            None,
            Some("reflection_fixes"),
        ),
        // Step 4: Load child task run summaries (reflections, follow-ups)
        build_api_step(
            "Load child workflow summaries",
            "GET",
            &format!(
                "{}/task-runs?parent_task_run_id={}&limit=20",
                base_url, source_task_run_id
            ),
            None,
            Some("child_summaries"),
        ),
    ]
}

/// Build verification steps for the fixer workflow.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    vec![
        {
            let mut step = ExecutionStepConfig {
                step_type: "prompt".to_string(),
                name: Some("Verify fixer fix attempts".to_string()),
                prompt_content: Some(
                    r#"Verify that the fixer analysis is complete.
DO NOT use any UI Bridge SDK tools — they are not available in fixer mode.
Base your verification ONLY on the data already loaded in runtime variables and your own output.

Check:
1. The AI identified unfixed issues from reflections and follow-ups
2. Each identified issue was either fixed ([FIXER_FIX]) or explicitly skipped ([FIXER_SKIP])
3. Fix attempts include clear descriptions of what was changed

If no unfixed issues were found across all child outputs, that is acceptable — confirm this.
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

/// Build the agentic phase prompt for the fixer AI.
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a fixer agent completing ALL remaining work from the workflow "{}" and its child workflows (reflections, follow-ups).

Your goal is to read every reflection and follow-up output, find what's still unfixed, and implement it.

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_ai_output}}}}` — The complete AI conversation output from the source run
- `{{{{source_findings}}}}` — Categorized findings from the source run
- `{{{{reflection_fixes}}}}` — Reflection fix records with REFLECTION_FIX markers and status
- `{{{{child_summaries}}}}` — Summaries of all child task runs (reflections, follow-ups)
- `{{{{referenced_files}}}}` — File paths referenced in findings and AI output (validated as existing)
- `{{{{referenced_file_contents}}}}` — Pre-loaded contents of referenced files
- `{{{{project_structure}}}}` — Condensed directory tree of the project

## Working Directory

The project root is: {{{{project_root}}}}
All relative file paths in findings are relative to this directory.

## Efficiency Guidelines

1. **Start with pre-loaded files.** The referenced file contents contain the source files mentioned in findings. Read them FIRST before making any tool calls.
2. **Never read the same file twice.** If you already have a file's contents (pre-loaded or via tool), do NOT read it again.
3. **Use the project structure** below to navigate instead of running find, ls, or blind searches.
4. **Use targeted grep** with specific patterns rather than exploratory find/ls commands.

## Pre-loaded File Contents

{{{{referenced_file_contents}}}}

## Project Structure

{{{{project_structure}}}}

## Your Task

1. **Read the reflection_fixes data** — identify all REFLECTION_FIX entries that have NOT been applied
2. **Read child_summaries** — look for FOLLOW_UP_FIX and FOLLOW_UP_SKIP markers in follow-up outputs
3. **Cross-reference** — determine what's still unfixed after all reflections and follow-ups ran
4. **For each remaining unfixed issue, implement the fix** — you have full access to the codebase and tools
5. **Mark each issue** with fixer markers

### Fix Markers

For each issue you successfully fix:
```
[FIXER_FIX]
Issue: Brief description of the unfixed issue
Source: Which child workflow identified it (reflection/follow-up ID or name)
Fix: What you did to fix it
Files: list of files modified
[/FIXER_FIX]
```

For each issue you cannot fix or choose to skip:
```
[FIXER_SKIP]
Issue: Brief description of the unfixed issue
Source: Which child workflow identified it
Reason: Why it cannot be fixed (e.g., requires external dependency, needs human decision, etc.)
[/FIXER_SKIP]
```

## Important Guidelines

- **DO fix actual code issues** — you should make real code changes
- **DO NOT repeat work already done** — if a reflection or follow-up already fixed something, skip it
- **Focus on what's left** — items marked REFLECTION_FIX but not applied, FOLLOW_UP_SKIP items that are actually fixable
- **Be practical** — if an issue truly requires human decision-making or external access, use [FIXER_SKIP]
- **Keep changes minimal** — fix only the specific unfixed issues, don't refactor surrounding code
- **Test your fixes** — run relevant tests or verification after making changes

## Output Structure

Start with a cross-reference summary:
```
## Unfixed Issues Remaining After Reflections & Follow-Ups
1. [issue description] — Source: [reflection/follow-up name] — Status: WILL_FIX / WILL_SKIP
2. [issue description] — Source: [reflection/follow-up name] — Status: WILL_FIX / WILL_SKIP
...
```

Then proceed to fix each WILL_FIX item, marking with [FIXER_FIX] as you go.
Finally, mark any WILL_SKIP items with [FIXER_SKIP].

End with a summary:
```
## Fixer Summary
- Unfixed issues found: N
- Issues fixed by fixer: N
- Issues skipped by fixer: N
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
