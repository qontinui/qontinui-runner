//! Programmatic reflection workflow definition.
//!
//! Builds the execution steps for a reflection workflow, including:
//! - Setup phase: Load source run data via curl commands
//! - Agentic phase: AI analyzes the data and applies fixes
//! - Verification phase: Verify no destructive changes
//! - Completion phase: Record patterns and evaluate effectiveness

use std::collections::HashMap;

use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::LoopConfig;

/// Build the LoopConfig for a reflection workflow.
pub fn build_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 3,
        base_prompt: build_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true, // Run analysis before verification
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
        enable_sweep: false,
        max_sweep_iterations: 5,
        stages: Vec::new(),
        stop_on_failure: false,
        reflection_mode: true,
        provider_override: None,
        model_override: None,
        stage_index: None,
    }
}

/// Build setup steps that load source run data via curl commands.
pub fn build_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
) -> Vec<ExecutionStepConfig> {
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
pub fn build_verification_steps(source_task_run_id: &str) -> Vec<ExecutionStepConfig> {
    let base_url = "http://localhost:9876";

    vec![
        // Step 1: Verify reflection fixes were recorded via API
        {
            let mut step = ExecutionStepConfig {
                step_type: "command".to_string(),
                command_mode: Some("check".to_string()),
                name: Some("Verify reflection data accessible".to_string()),
                check_type: Some("http_status".to_string()),
                check_url: Some(format!(
                    "{}/task-runs/{}/knowledge",
                    base_url, source_task_run_id
                )),
                expected_status: Some(200),
                ..Default::default()
            };
            step.phase = Some("verification".to_string());
            step
        },
        // Step 2: Lightweight prompt check (no UI Bridge dependency)
        build_verification_prompt_step(
            "Verify reflection safety",
            r#"Verify that the reflection analysis is complete and no destructive changes were applied.
DO NOT use any UI Bridge SDK tools — they are not available in reflection mode.
Base your verification ONLY on the data already loaded in runtime variables and your own output.

Check:
1. All significant findings from the source run were analyzed
2. No files were deleted or overwritten without recording the change
3. All applied fixes have been recorded via [REFLECTION_FIX:...] markers
4. Fix descriptions are clear and actionable

If any issues are found, report them. Otherwise, confirm the reflection is safe."#,
        ),
    ]
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
///
/// Dispatches to a generation-specific or execution-specific prompt based on
/// whether the source workflow was a generation meta-workflow ("AI Generate: ...").
fn build_agentic_prompt(source_workflow_name: &str) -> String {
    let is_generation = source_workflow_name.starts_with("AI Generate:");

    let preamble = format!(
        r#"You are a reflection agent analyzing the completed {} for "{}".

{}

## CRITICAL: Analysis Only — No File System Access

You MUST NOT use any file system tools (find, grep, cat, ls, read, glob, etc.) or explore the codebase.
All the data you need has already been loaded into runtime variables below.
Your job is to ANALYZE the provided data and produce REFLECTION_FIX markers — not to explore or modify files.
Do NOT run any bash commands. Focus entirely on analyzing the data and writing your analysis."#,
        if is_generation {
            "workflow generation run"
        } else {
            "workflow run"
        },
        source_workflow_name,
        if is_generation {
            "Your goal is to identify systemic issues in the workflow generation pipeline and apply fixes \
             that will improve the quality of future generated workflows."
        } else {
            "Your goal is to identify systemic issues and apply fixes that will improve subsequent runs."
        },
    );

    let data_section = r#"
## Data Available

The setup phase loaded the following data into runtime variables:
- `{{source_findings}}` — Categorized findings with signature hashes
- `{{source_knowledge}}` — Observations, solutions, root causes recorded during execution
- `{{source_ai_output}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{source_workflow_state}}` — Workflow execution state (phases, iterations, timing)
- `{{previous_fixes}}` — Previous reflection fixes for this workflow (for effectiveness comparison)"#;

    let marker_section = r#"
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

**confidence** must be one of: `high`, `medium`, `low`"#;

    let analysis_steps = if is_generation {
        build_generation_analysis_steps()
    } else {
        build_execution_analysis_steps()
    };

    let evaluation_section = r#"
### Step 5: Evaluate Previous Fixes
Compare the source run's findings against previously applied fixes:
- If a fix's source finding signature does NOT appear in this run → fix was effective
- If the signature DOES appear → fix was ineffective
- If new findings appeared that weren't present before → possible regression

Note: Batch effectiveness evaluation runs automatically in the completion phase.
Focus your analysis on qualitative observations about which fixes helped and which didn't,
and record any new fixes needed using `[REFLECTION_FIX:...]` markers."#;

    format!(
        "{}\n{}\n{}\n{}\n{}",
        preamble, data_section, marker_section, analysis_steps, evaluation_section
    )
}

/// Analysis steps for reflecting on a workflow execution run.
///
/// Focuses on runtime issues: tool failures, selector problems, missing context,
/// wasted iterations, and decision quality during execution.
fn build_execution_analysis_steps() -> String {
    r#"
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
The following fix types are automatically applied by the system:
- `knowledge_base_update` at high/medium confidence → Creates a recurring_pattern knowledge entry
- `context_addition` at high confidence → Creates a context knowledge entry
- `instruction_clarification` at high confidence → Creates a new generation rule for the relevant prompt agent (schema_context, hardener, or verification). Use this for insights about how generation prompts should be improved.
- `workflow_step_rewrite` at high confidence → Creates a new generation rule for the relevant prompt agent. Use this for patterns about how specific step types should be generated differently.

All other fix types are recorded for manual review. Use these auto-applied types when you have
clear, actionable insights that should persist across runs. Avoid low-confidence auto-applied types
as they will only be recorded without creating knowledge entries.

### Deduplication
Fixes are deduplicated by content hash (fix_type + description + old_value + new_value).
If an identical fix already exists with status 'applied', the duplicate will be skipped.
Only emit fixes for genuinely new insights — do NOT re-emit fixes from previous reflection runs."#
        .to_string()
}

/// Analysis steps for reflecting on a workflow generation run.
///
/// Focuses on generation quality: prompt interpretation, step type selection,
/// builder/verifier/fixer agent quality, and structural issues in the output workflow.
fn build_generation_analysis_steps() -> String {
    r#"
### Example

```
[REFLECTION_FIX:instruction_clarification:high]
Description: Builder agent consistently generates command steps with shell_command instead of using the prompt step type for AI-driven analysis tasks. The builder prompt should clarify when to use prompt vs command steps.
Old: No guidance on prompt vs command step selection
New: Added rule: use prompt steps when the task requires AI reasoning/analysis; use command steps only for deterministic shell operations
[/REFLECTION_FIX]
```

### API Fallback

If you need to make HTTP calls directly (e.g., to evaluate previous fix effectiveness),
use the `api_request` MCP tool to call the runner API at http://localhost:9876.

## Analysis Steps

The generation pipeline uses a 3-agent architecture: **Builder** (creates the initial workflow JSON), **Verifier** (reviews for correctness), and **Fixer** (corrects issues found by the verifier). Analyze each agent's performance.

### Step 1: Builder Agent Analysis (PRIMARY)
Read the AI conversation output end-to-end and evaluate the builder's decisions:
- **Prompt interpretation** — Did the builder correctly understand the user's intent from the description? Did it miss key requirements or add unnecessary steps?
- **Step type selection** — Were the right step types chosen (command vs prompt vs check)? Were there cases where a different step type would have been more appropriate?
- **Step configuration quality** — Were steps well-configured with appropriate commands, selectors, timeouts, and expected values? Were there obvious misconfigurations?
- **Workflow structure** — Is the phase organization logical (setup → verification → agentic → completion)? Are steps in the right phases?
- **Missing steps** — Were important steps omitted that the workflow clearly needs?
- **Unnecessary steps** — Were steps included that add no value or are redundant?

### Step 2: Verifier Agent Analysis
Evaluate the quality of the verifier's review:
- **False positives** — Did the verifier flag issues that weren't actually problems, wasting fixer iterations?
- **False negatives** — Did real issues slip through that the verifier should have caught?
- **Feedback quality** — Was the verifier's feedback specific and actionable, or vague and unhelpful?
- **Verification coverage** — Did the verifier check all important aspects (step types, configuration, phase assignments, naming)?

### Step 3: Fixer Agent Analysis
Evaluate the fixer's corrections:
- **Fix quality** — Did fixes actually improve the workflow, or did they introduce new issues?
- **Over-correction** — Did the fixer make unnecessary changes beyond what the verifier flagged?
- **Under-correction** — Did the fixer fail to address issues the verifier identified?
- **Iteration efficiency** — How many fix iterations were needed? Could the workflow have converged faster?

### Step 4: Categorize Issues and Apply Fixes
For each identified issue, determine the best fix type:

- `instruction_clarification` (HIGH PRIORITY for generation) — Improvements to builder/verifier/fixer prompts. These become generation rules that directly improve future workflow generation.
- `workflow_step_rewrite` (HIGH PRIORITY for generation) — Patterns about how specific step types should be generated. These become step-type-specific generation rules.
- `knowledge_base_update` — Recurring generation anti-patterns that should be remembered across runs.
- `context_addition` — Missing context that the generation agents needed (e.g., available step types, configuration schemas, platform capabilities).

Record each fix using a `[REFLECTION_FIX:...]` marker. The source and reflection task run IDs are filled in automatically.

**Confidence guidelines:**
- `high` — Clear pattern seen in the output, straightforward prompt improvement
- `medium` — Likely improvement but based on a single observation
- `low` — Speculative, may need more data points

### Auto-Applied Fix Types
The following fix types are automatically applied by the system:
- `knowledge_base_update` at high/medium confidence → Creates a recurring_pattern knowledge entry
- `context_addition` at high confidence → Creates a context knowledge entry
- `instruction_clarification` at high confidence → Creates a new generation rule for the relevant prompt agent (schema_context, hardener, or verification). **This is the most valuable fix type for generation reflection** — it directly improves the prompts used by the builder, verifier, and fixer agents.
- `workflow_step_rewrite` at high confidence → Creates a new generation rule for the relevant prompt agent. Use this for patterns about how specific step types should be generated differently.

All other fix types are recorded for manual review.

### Deduplication
Fixes are deduplicated by content hash (fix_type + description + old_value + new_value).
If an identical fix already exists with status 'applied', the duplicate will be skipped.
Only emit fixes for genuinely new insights — do NOT re-emit fixes from previous reflection runs."#
        .to_string()
}

/// Helper: Build a command step that makes an HTTP request via curl.
///
/// Constructs a `command` step with a curl shell command.
/// The `output_variable` is extracted from the step output by the runtime.
fn build_api_step(
    name: &str,
    method: &str,
    url: &str,
    body: Option<&str>,
    output_variable: Option<&str>,
    is_setup: bool,
) -> ExecutionStepConfig {
    // Build curl command
    let curl_cmd = if let Some(body_str) = body {
        format!(
            "curl -sf -X {} -H \"Content-Type: application/json\" -d '{}' '{}'",
            method, body_str, url
        )
    } else {
        format!("curl -sf -X {} '{}'", method, url)
    };

    // If we need to capture output into a variable, use extract
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
    if is_setup {
        step.phase = Some("setup".to_string());
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
