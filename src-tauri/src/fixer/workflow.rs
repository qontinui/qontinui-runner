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
        max_context_tokens: 500_000,
        enforce_token_budget: false,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
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
                "{}/task-runs/{}/reflection-fixes",
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
    vec![{
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
    }]
}

/// Build the agentic phase prompt for the fixer AI.
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a fixer agent for the workflow "{}" and its child workflows (reflections, follow-ups).

You have two jobs:
1. **Complete remaining work** — find what reflections and follow-ups identified but didn't finish, and implement it.
2. **Think critically about what was missed** — reflections and follow-ups may have diagnosed symptoms without finding root causes, proposed workarounds instead of real fixes, or missed issues entirely. You should go deeper.

You have full access to all repos and tools. You are not limited to small patches — if the right fix requires changes across multiple files or architectural understanding, do it.

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

### Step 1: Understand What Happened

Read the source AI output, findings, reflection fixes, and child summaries end-to-end. Build a mental model of:
- What the original workflow was trying to do
- What failed and why
- What the reflections diagnosed and fixed
- What the follow-ups attempted

### Step 2: Find Remaining Unfixed Work

Identify all REFLECTION_FIX entries that have NOT been applied, and FOLLOW_UP_SKIP items that are actually fixable.

### Step 3: Think Critically

Before implementing, ask yourself:
- **Did the reflection find the root cause, or just the symptom?** If a reflection said "endpoint returns empty data, add a retry" — that's a symptom fix. The root cause might be a stale cache, a memoization bug, or a broken data flow. Trace the code path to find out.
- **Are there related issues the reflection missed?** If one endpoint has a bug, similar endpoints likely have the same bug. Check them.
- **Did the reflection propose a workaround when a real fix exists?** Workarounds (retries, fallbacks, "use the other endpoint instead") should be replaced with actual fixes when possible.
- **Is there a pattern across multiple issues?** Multiple symptoms may share a single root cause. Fix the cause once rather than patching each symptom.

When you find a deeper issue, trace it through the codebase. Read the relevant source files, understand the data flow, and implement the correct fix. Do not limit yourself to what the reflection explicitly identified — if you discover a bug while investigating, fix it.

### Step 4: Implement Fixes

For each issue, implement the fix directly in the codebase. You have full tool access.

After making changes, verify them:
- TypeScript: `cd <package> && npx tsc --noEmit && npm run build`
- Rust: `cd qontinui-runner/src-tauri && cargo check`
- If the fix affects UI Bridge endpoints, use curl to verify the endpoint now returns correct data

### Step 5: Record What You Did

Mark each fix with structured markers.

For each issue you successfully fix:
```
[FIXER_FIX]
Issue: Brief description of the unfixed issue
Source: Which child workflow identified it, or "fixer-discovered" if you found it independently
Root Cause: What was actually wrong (may differ from what the reflection diagnosed)
Fix: What you did to fix it
Files: list of files modified
Verified: How you verified the fix works
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

- **Think before you patch.** Understand the architecture before making changes. Read the relevant code paths end-to-end.
- **Fix root causes, not symptoms.** If something returns empty data, find out why — don't add retries or fallbacks around it.
- **Prefer robust, scalable solutions over quick fixes.** A quick fix solves today's problem; a robust fix prevents the entire class of problem. For example: if one endpoint reads stale cached data, don't just fix that endpoint — find every endpoint with the same pattern and fix them all. If an error message is unhelpful, don't just improve the string — make the error include actionable context that helps the AI self-correct.
- **Follow the data flow.** Most bugs live at layer boundaries. Trace data from its source (registry, database, API) through each transformation to where it's consumed.
- **Broad changes are fine when correct.** If the right fix touches 5 files across 3 packages, do it. A correct multi-file fix is better than a narrow patch that leaves the underlying issue.
- **DO NOT repeat work already done** — if a reflection or follow-up already fixed something and it's verified working, skip it.
- **Test your fixes** — run compilation checks and, where possible, verify the fix via the relevant API endpoints.
- **Be honest about what you can't fix** — use FIXER_SKIP for issues that genuinely require human judgment or external access.

## Output Structure

Start with a critical assessment:
```
## Assessment
### What Reflections Got Right
- [list of correctly diagnosed and fixed issues]

### What Reflections Missed or Got Wrong
- [list of issues where the reflection diagnosed symptoms, proposed workarounds, or missed the root cause]

### New Issues Discovered
- [issues you found while investigating that no prior workflow identified]
```

Then list your plan:
```
## Unfixed Issues
1. [issue] — Source: [reflection/follow-up/fixer-discovered] — Status: WILL_FIX / WILL_SKIP
...
```

Then implement each fix, marking with [FIXER_FIX] as you go.

End with:
```
## Fixer Summary
- Issues inherited from reflections/follow-ups: N
- New issues discovered by fixer: N
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
