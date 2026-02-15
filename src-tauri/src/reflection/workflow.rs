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
            true,
        ),
        // Step 2: Load knowledge from source run
        build_api_step(
            "Load source knowledge",
            "GET",
            &format!("{}/task-runs/{}/knowledge", base_url, source_task_run_id),
            None,
            Some("source_knowledge"),
            true,
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
            true,
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
            true,
        ),
        // Step 5: Load previous reflection fixes for effectiveness evaluation
        // Filter to status=applied to exclude superseded duplicates (reduces context from ~40 to ~15 entries)
        build_api_step(
            "Load previous fixes",
            "GET",
            &format!(
                "{}/reflection-fixes?workflow_name={}&status=applied",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("previous_fixes"),
            true,
        ),
    ]
}

/// Build verification steps for the reflection workflow.
pub fn build_verification_steps() -> Vec<ExecutionStepConfig> {
    // Reflection verification is lightweight — just ensure no destructive changes
    vec![build_verification_prompt_step(
        "Verify reflection safety",
        r#"Verify that the reflection analysis is complete and no destructive changes were applied.

Check:
1. All significant findings from the source run were analyzed
2. No files were deleted or overwritten without recording the change
3. All applied fixes have been recorded via [REFLECTION_FIX:...] markers
4. Fix descriptions are clear and actionable

If any issues are found, report them. Otherwise, confirm the reflection is safe."#,
    )]
}

/// Build completion steps for the reflection workflow.
pub fn build_completion_steps(source_workflow_name: &str) -> Vec<ExecutionStepConfig> {
    let base_url = "http://localhost:9876";

    vec![
        // Step 1: Batch evaluate previous fixes (automation step)
        build_api_step(
            "Evaluate fix effectiveness",
            "POST",
            &format!(
                "{}/reflection/evaluate?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("evaluation_results"),
            false,
        ),
        // Step 2: AI summary and pattern recording (prompt step)
        {
            let mut step = build_prompt_step(
                "Record patterns and summarize",
                r#"Complete the reflection by:

1. Record any recurring patterns as knowledge entries using the standard [KNOWLEDGE:recurring_pattern] markers
2. The batch effectiveness evaluation has already been run. Results: {{evaluation_results}}
3. Generate a brief summary of:
   - Issues identified (count by category)
   - Fixes applied (count by type)
   - Previous fix effectiveness results
   - Recommendations for the next run"#,
            );
            step.phase = Some("completion".to_string());
            step
        },
    ]
}

/// Build the agentic phase prompt for the reflection AI.
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a reflection agent analyzing the completed workflow run for "{}".

Your goal is to identify systemic issues and apply fixes that will improve subsequent runs.

## CRITICAL: Analysis Only — No File System Access

You MUST NOT use any file system tools (find, grep, cat, ls, read, glob, etc.) or explore the codebase.
All the data you need has already been loaded into runtime variables below.
Your job is to ANALYZE the provided data and produce REFLECTION_FIX markers — not to explore or modify files.
Do NOT run any bash commands. Focus entirely on analyzing the data and writing your analysis.

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_findings}}}}` — Categorized findings with signature hashes
- `{{{{source_knowledge}}}}` — Observations, solutions, root causes recorded during execution
- `{{{{source_ai_output}}}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state (phases, iterations, timing)
- `{{{{previous_fixes}}}}` — Previous reflection fixes for this workflow (for effectiveness comparison)

## Recording Fixes

Use `[REFLECTION_FIX:...]` markers to record each fix you apply. These are parsed automatically
from your output — no HTTP calls needed.

### Marker Format

```
[REFLECTION_FIX:fix_type:confidence]
Description: What was changed and why
File: optional/path/to/file.ext
Old: optional previous value
New: optional new value
Finding: optional-source-finding-id
[/REFLECTION_FIX]
```

**fix_type** must be one of: `knowledge_base_update`, `workflow_step_rewrite`, `selector_fix`, `tool_config_update`, `context_addition`, `instruction_clarification`
(shortcuts: `kb_update`, `step_rewrite`, `selector`, `tool_config`, `context`, `clarification`)

**confidence** must be one of: `high`, `medium`, `low`

### Example

```
[REFLECTION_FIX:context_addition:high]
Description: Added missing workspace root path to the setup context so the AI knows where project files are located
File: workflows/my-workflow.json
Old: No workspace context provided
New: Added workspace_root variable to setup phase
[/REFLECTION_FIX]
```

### API Fallback

If you need to make HTTP calls directly (e.g., to evaluate previous fix effectiveness),
use the `api_request` MCP tool to call the runner API at http://localhost:9876.

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
For each actionable issue, apply the fix and record it using a `[REFLECTION_FIX:...]` marker.
The source and reflection task run IDs are filled in automatically — just provide the fix details.

Example:
```
[REFLECTION_FIX:selector_fix:high]
Description: Updated login button selector from #btn-login to button[data-testid="login"] because the old ID was removed in a recent refactor
File: workflows/login-flow.json
Old: #btn-login
New: button[data-testid="login"]
Finding: finding-abc123
[/REFLECTION_FIX]
```

**Confidence guidelines:**
- `high` — Clear root cause, straightforward fix, apply automatically
- `medium` — Likely fix but needs validation, apply and monitor
- `low` — Speculative, log but don't auto-apply

### Auto-Applied Fix Types
The following fix types are automatically applied by the system (creating knowledge base entries):
- `knowledge_base_update` at high/medium confidence → Creates a recurring_pattern knowledge entry
- `context_addition` at high confidence → Creates a context knowledge entry

All other fix types are recorded for manual review. Use these auto-applied types when you have
clear, actionable insights that should persist across runs. Avoid low-confidence auto-applied types
as they will only be recorded without creating knowledge entries.

### Deduplication
Fixes are deduplicated by content hash (fix_type + description + old_value + new_value).
If an identical fix already exists with status 'applied', the duplicate will be skipped.
Only emit fixes for genuinely new insights — do NOT re-emit fixes from previous reflection runs.

### Step 5: Evaluate Previous Fixes
Compare the source run's findings against previously applied fixes:
- If a fix's source finding signature does NOT appear in this run → fix was effective
- If the signature DOES appear → fix was ineffective
- If new findings appeared that weren't present before → possible regression

Note: Batch effectiveness evaluation runs automatically in the completion phase.
Focus your analysis on qualitative observations about which fixes helped and which didn't,
and record any new fixes needed using `[REFLECTION_FIX:...]` markers."#,
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
    is_setup: bool,
) -> ExecutionStepConfig {
    let mut step = ExecutionStepConfig {
        step_type: "api_request".to_string(),
        name: Some(name.to_string()),
        api_method: Some(method.to_string()),
        api_url: Some(url.to_string()),
        api_body: body.map(|b| b.to_string()),
        api_output_variable: output_variable.map(|v| v.to_string()),
        ..Default::default()
    };
    if is_setup {
        step.phase = Some("setup".to_string());
        step.is_setup = Some(true);
        step.run_on_subsequent_iterations = Some(false);
    } else {
        step.phase = Some("completion".to_string());
    }
    step
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

/// Helper: Build a verification prompt step.
fn build_verification_prompt_step(name: &str, content: &str) -> ExecutionStepConfig {
    let mut step = build_prompt_step(name, content);
    step.phase = Some("verification".to_string());
    step
}
