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
        constraint_overrides: std::collections::HashMap::new(),
        reflection_mode: true,
        provider_override: None,
        model_override: None,
        model_overrides: std::collections::HashMap::new(),
        stage_index: None,
        max_sessions: Some(3),
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
pub fn build_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

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
        // Step 6: Load structured already-tried summary (all fixes, grouped by type with DO NOT retry warnings)
        build_api_step(
            "Load already-tried summary",
            "GET",
            &format!(
                "{}/reflection/already-tried-summary?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("already_tried_summary"),
            true,
        ),
    ]
}

/// Build verification steps for the reflection workflow.
pub fn build_verification_steps(source_task_run_id: &str) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

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
pub fn build_completion_steps(
    source_workflow_name: &str,
    source_task_run_id: &str,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

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
        // Step 2: Build automated causal links from this run's data
        build_api_step(
            "Build causal links",
            "POST",
            &format!(
                "{}/reflection/build-causal-links?task_run_id={}&workflow_name={}",
                base_url,
                source_task_run_id,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("causal_link_results"),
            false,
        ),
        // Step 3: Rebuild architecture model
        build_api_step(
            "Rebuild architecture model",
            "POST",
            &format!(
                "{}/reflection/architecture/rebuild?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("architecture_rebuild_results"),
            false,
        ),
        // Step 4: AI summary and pattern recording (prompt step)
        {
            let mut step = build_prompt_step(
                "Record patterns and summarize",
                r#"Complete the reflection by:

1. Record any recurring patterns as knowledge entries using the standard [KNOWLEDGE:recurring_pattern] markers
2. The batch effectiveness evaluation has already been run. Results: {{evaluation_results}}
3. Causal links built: {{causal_link_results}}
4. Architecture model rebuilt: {{architecture_rebuild_results}}
5. Generate a brief summary of:
   - Issues identified (count by category)
   - Fixes applied (count by type)
   - Previous fix effectiveness results
   - Causal relationships identified
   - Architecture model status (components & relationships)
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

## Tool Access

You have full tool access (file read/write, bash, grep, etc.). Your primary job is to analyze
the loaded data and produce REFLECTION_FIX markers, but you may also explore the codebase
when you need to investigate root causes, verify assumptions, or apply fixes directly."#,
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
- `{{previous_fixes}}` — Previous reflection fixes for this workflow (for effectiveness comparison)
- `{{already_tried_summary}}` — Structured summary of ALL prior fixes grouped by type, with effectiveness labels and DO NOT retry warnings

## CRITICAL: Already-Tried Context

Review the already-tried summary (`{{already_tried_summary}}`) BEFORE proposing any fixes.
Do NOT re-attempt approaches marked as FAILED or REGRESSION. If a fix type has low effectiveness
across prior attempts, try a fundamentally different strategy rather than variations of the same approach."#;

    let marker_section = r#"
## Recording Fixes

Use `[REFLECTION_FIX:...]` markers to record each fix you apply. These are parsed automatically
from your output — no HTTP calls needed.

### Marker Format

```
[REFLECTION_FIX:fix_type:confidence]
Description: What was changed and why
Reasoning: Root cause diagnosis and evidence (optional but recommended)
Alternatives: Other approaches considered and why they were rejected (optional)
Scope: optional scope — 'universal' for patterns that apply across all projects (default: auto-detected)
Applicability: context for when this pattern applies (required when Scope is universal)
File: optional/path/to/file.ext
Old: optional previous value
New: optional new value
Finding: optional-source-finding-id
[/REFLECTION_FIX]
```

**fix_type** must be one of: `knowledge_base_update`, `workflow_step_rewrite`, `selector_fix`, `tool_config_update`, `context_addition`, `instruction_clarification`
(shortcuts: `kb_update`, `step_rewrite`, `selector`, `tool_config`, `context`, `clarification`)

**confidence** must be one of: `high`, `medium`, `low`

**Decision Context:** For each fix, explain WHY in the `Reasoning:` field (root cause diagnosis, evidence from the conversation output, how you identified the issue). Use `Alternatives:` when you considered multiple approaches — document what you weighed and why you chose this approach over others. This structured reasoning is surfaced during effectiveness evaluation when a fix later proves ineffective.

### Universal Patterns

When you identify a pattern that is NOT specific to this project's code or structure,
mark it with `Scope: universal`. Examples:
- "Always use data-testid selectors instead of CSS classes for test automation"
- "Wait for network idle before asserting page content"
- "Set explicit timeouts for CI environments vs local"

Do NOT mark as universal:
- Project-specific file paths or directory structures
- Patterns that depend on a specific codebase's conventions

When marking a fix as universal, always include `Applicability:` describing
when this pattern is relevant for other projects."#;

    let analysis_steps = if is_generation {
        build_generation_analysis_steps()
    } else {
        build_execution_analysis_steps()
    };

    let causal_section = r#"
## Causal Chain Analysis

In addition to recording fixes, identify causal relationships between events.
Use `[CAUSAL_CHAIN:...]` markers to record cause→effect links you observe.

### Marker Format

```
[CAUSAL_CHAIN:relationship]
Cause: event_type:reference
Effect: event_type:reference
Description: What caused what and why
[/CAUSAL_CHAIN]
```

**relationship** must be one of: `caused`, `triggered`, `resolved`, `prevented`

**event_type** must be one of: `code_change`, `finding_detected`, `error_occurred`, `fix_applied`, `verification_passed`, `verification_failed`

### What to look for
- Code changes that caused test failures or errors
- Errors that triggered specific findings
- Fixes that resolved specific errors
- Changes that prevented previously-recurring issues

### Example
```
[CAUSAL_CHAIN:caused]
Cause: code_change:src/api/routes.ts
Effect: finding_detected:API endpoint returns 404
Description: Route handler was moved to a new file but the import path wasn't updated
[/CAUSAL_CHAIN]
```

Focus on the most significant causal relationships (max 5). Don't record trivial ones."#;

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
        "{}\n{}\n{}\n{}\n{}\n{}",
        preamble, data_section, marker_section, causal_section, analysis_steps, evaluation_section
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
Reasoning: The AI spent 3 iterations searching for project files because it didn't know the workspace root. The conversation output shows repeated 'find' commands across different directories before eventually locating the project at /home/user/project.
Alternatives: Considered adding a file listing step instead, but providing the root path is simpler and lets the AI explore as needed rather than pre-loading a potentially stale file list.
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
Description: Updated login button selector from #btn-login to button[data-testid="login"]
Reasoning: The old #btn-login ID was removed in a recent frontend refactor (commit abc1234). The conversation output shows the AI tried this selector 3 times with timeouts before falling back to a text-based search.
Alternatives: Considered using button.login-btn class selector, but data-testid attributes are explicitly maintained for testing and are more stable across CSS changes.
Scope: universal
Applicability: Web apps with test automation — prefer data-testid attributes over CSS selectors
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
Description: Builder agent generates command steps instead of prompt steps for AI-driven analysis tasks
Reasoning: In 3 of 5 generated workflows, the builder used shell_command steps for tasks requiring AI reasoning (e.g., "analyze code quality"). The conversation shows the verifier flagged this twice but the fixer only corrected one instance, suggesting the builder prompt lacks clear guidance.
Alternatives: Considered adding a post-generation validation rule to auto-convert mistyped steps, but fixing the builder prompt at the source is more efficient and prevents the issue entirely.
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

// =============================================================================
// Project Reflection Workflow
// =============================================================================
// Learns about the user's project/codebase — environment, architecture, test
// patterns, recurring issues. Runs in both dev and production modes.

/// Build the LoopConfig for a project reflection workflow.
pub fn build_project_reflection_config(
    execution_id: &str,
    workflow_name: &str,
    source_workflow_name: &str,
    project_path: Option<String>,
) -> LoopConfig {
    LoopConfig {
        max_iterations: 2, // Project reflection needs fewer iterations
        base_prompt: build_project_agentic_prompt(source_workflow_name),
        workflow_name: workflow_name.to_string(),
        workflow_id: format!("project-reflection-{}", execution_id),
        execution_id: execution_id.to_string(),
        targeted_error_ids: Vec::new(),
        starting_iteration: 0,
        run_agentic_first: true,
        artifact_dir: None,
        is_dev_mode: false, // CRITICAL: Prevents cascade reflection
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
        max_sessions: Some(2),
        auto_run_generated: false,
        approval_gate: false,
        max_context_tokens: 80_000,
        cross_workflow_learning: false, // Not needed — project reflection IS the cross-project learner
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path,
    }
}

/// Build setup steps for project reflection (loads source run data).
pub fn build_project_setup_steps(
    source_task_run_id: &str,
    source_workflow_name: &str,
) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

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
        // Step 3: Load AI conversation output
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
        // Step 5: Load previous project-scoped fixes
        build_api_step(
            "Load previous project fixes",
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
        // Step 6: Load structured already-tried summary
        build_api_step(
            "Load already-tried summary",
            "GET",
            &format!(
                "{}/reflection/already-tried-summary?workflow_name={}",
                base_url,
                urlencoding::encode(source_workflow_name)
            ),
            None,
            Some("already_tried_summary"),
            true,
        ),
    ]
}

/// Build verification steps for project reflection (simplified — safety check only).
pub fn build_project_verification_steps(source_task_run_id: &str) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Single HTTP health check
        {
            let mut step = ExecutionStepConfig {
                step_type: "command".to_string(),
                command_mode: Some("check".to_string()),
                name: Some("Verify source data accessible".to_string()),
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
    ]
}

/// Build completion steps for project reflection.
pub fn build_project_completion_steps(source_workflow_name: &str) -> Vec<ExecutionStepConfig> {
    let base_url = crate::mcp::types::get_self_base_url_from_env();

    vec![
        // Step 1: Batch evaluate previous fixes
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
        // Step 2: AI summary
        {
            let mut step = build_prompt_step(
                "Summarize project learnings",
                r#"Summarize what was learned about this project:

1. List the project knowledge entries you recorded (by category)
2. Batch effectiveness results: {{evaluation_results}}
3. Brief assessment: Are there important aspects of the project still not documented?

Keep the summary concise — this is for internal tracking, not user display."#,
            );
            step.phase = Some("completion".to_string());
            step
        },
    ]
}

/// Build the agentic prompt for project reflection.
///
/// Unlike workflow reflection which focuses on fixing workflow mechanics,
/// project reflection focuses on learning about the user's project/codebase.
fn build_project_agentic_prompt(source_workflow_name: &str) -> String {
    format!(
        r#"You are a project reflection agent analyzing the completed workflow run for "{}".

Your goal is NOT to fix workflows — instead, you are learning about the **user's project**.
Extract lasting knowledge about the project's environment, architecture, test patterns,
and recurring issues. This knowledge will be injected into future workflows targeting
the same project directory.

## Tool Access

You have full tool access (file read/write, bash, grep, etc.). Use it to explore the
project directory when the AI output suggests the workflow struggled with something
project-specific (e.g., "couldn't find the test runner", "wrong working directory").

## Data Available

The setup phase loaded the following data into runtime variables:
- `{{{{source_findings}}}}` — Categorized findings from the workflow run
- `{{{{source_knowledge}}}}` — Knowledge recorded during execution
- `{{{{source_ai_output}}}}` — The complete AI conversation output (CRITICAL — read this end-to-end)
- `{{{{source_workflow_state}}}}` — Workflow execution state
- `{{{{previous_fixes}}}}` — Previous project reflection knowledge (for deduplication)
- `{{{{already_tried_summary}}}}` — Structured summary of ALL prior fixes grouped by type, with effectiveness labels and DO NOT retry warnings

## CRITICAL: Already-Tried Context

Review the already-tried summary (`{{{{already_tried_summary}}}}`) BEFORE proposing any fixes.
Do NOT re-attempt approaches marked as FAILED or REGRESSION. If a fix type has low effectiveness
across prior attempts, try a fundamentally different strategy rather than variations of the same approach.

## Recording Project Knowledge

Use `[REFLECTION_FIX:...]` markers with the **project-scoped fix types**:

### Fix Types

| Type | Shortcut | Use for |
|------|----------|---------|
| `project_environment` | `proj_env` | Dependencies, env vars, runtime versions, required services |
| `project_architecture` | `proj_arch` | Where tests live, routes location, framework patterns, build system |
| `project_test_pattern` | `proj_test` | Test runner, fixture patterns, setup/teardown, docker requirements |
| `project_recurring_issue` | `proj_issue` | Flaky areas, slow operations, known quirks, common failure modes |

### Marker Format

```
[REFLECTION_FIX:project_environment:high]
Description: Project uses pnpm (not npm). Package install commands must use `pnpm install`.
[/REFLECTION_FIX]
```

**confidence** must be: `high`, `medium`, or `low`

## Analysis Steps

### Step 1: Read the AI Conversation Output (PRIMARY)
Read `{{{{source_ai_output}}}}` end-to-end. Look for:

1. **Environment requirements** the AI discovered or struggled with:
   - Did the AI fail because a dependency wasn't installed?
   - Did it use the wrong package manager, runtime version, or env var?
   - Were specific services (database, redis, etc.) required?

2. **Codebase structure** the AI had to figure out:
   - Did the AI search extensively for files? Where did it find them?
   - What framework patterns did it identify (e.g., "tests co-located with source")?
   - What build system is used? How are things organized?

3. **Test infrastructure** patterns:
   - What test runner is used? What fixture/setup patterns exist?
   - Are integration tests separate from unit tests? Do they need docker/services?
   - Are there conftest.py files, jest.config, vitest.config, etc.?

4. **Recurring friction points**:
   - Did the same type of error happen multiple times?
   - Were there flaky tests or timing-sensitive operations?
   - Were there known workarounds the AI had to discover?

### Step 2: Cross-reference with Findings
Check `{{{{source_findings}}}}` and `{{{{source_knowledge}}}}` for project-specific patterns
that the AI documented during execution.

### Step 3: Explore the Project (if needed)
If the AI output suggests the workflow struggled with project-specific issues,
use file tools to explore the project directory and verify your observations.

### Step 4: Record Project Knowledge
For each observation, emit a `[REFLECTION_FIX:...]` marker. Only emit genuinely
useful, project-specific knowledge. Do NOT emit:
- Workflow mechanics fixes (those belong in workflow reflection)
- Generic programming advice
- Things already in the previous fixes list

**Confidence guidelines:**
- `high` — Clearly observed in the AI output, verified by exploration
- `medium` — Inferred from the AI output, plausible but not directly confirmed
- `low` — Speculative, based on limited evidence

### Deduplication
Fixes are deduplicated by content hash. Check `{{{{previous_fixes}}}}` before emitting.
Only emit genuinely new insights — do NOT re-emit existing knowledge."#,
        source_workflow_name
    )
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
