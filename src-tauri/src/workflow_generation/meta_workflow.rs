//! Meta-Workflow Template Builder
//!
//! Builds a UnifiedWorkflow that implements the workflow generation pipeline
//! as a standard workflow. This makes the generation process visible,
//! editable, and forkable through the normal workflow system.
//!
//! Instead of the hardcoded 3-agent loop in `generator.rs`, this module
//! produces a UnifiedWorkflow whose phases map to the pipeline stages:
//!
//! - **Setup**: Builder agent generates initial workflow JSON
//! - **Verification**: AI review checks semantic correctness
//! - **Agentic**: Fixer agent corrects issues found by verification
//! - **Completion**: Saves the generated workflow to the library
//!
//! ## Enhanced prompts (v55+)
//!
//! When a database connection is available, prompts are enriched with:
//! - Historical generation patterns (common issues, success rates)
//! - Similar existing workflows as structural references
//! - Past successful fixes for the fixer agent

use rusqlite::Connection;
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use crate::unified_workflows::{LogSourceSelection, UnifiedWorkflow};
use crate::workflow_generation::schema_context::build_schema_context_for_description;

use super::self_improve;
use super::similar_workflows;
use super::GenerateWorkflowRequest;

/// Historical context retrieved from the database for prompt enhancement.
pub struct HistoricalContext {
    /// Formatted improvement context from self_improve.rs.
    pub improvement_section: String,
    /// Formatted similar workflows from similar_workflows.rs.
    pub similar_section: String,
    /// Top 5 most common past verification failures for the verifier.
    pub verifier_focus_items: Vec<String>,
    /// Formatted past successful fixes for the fixer.
    pub past_fixes_section: String,
    /// Formatted ground truth reference workflows with full JSON.
    pub gt_reference_section: String,
}

/// Build historical context from the database.
///
/// Returns None if there's no useful data or if queries fail.
pub fn build_historical_context(
    conn: &Connection,
    description: &str,
    query_embedding: Option<&[f32]>,
    category: Option<&str>,
) -> Option<HistoricalContext> {
    // 1. Self-improvement patterns
    let improvement_section = match self_improve::analyze_generation_patterns(conn) {
        Ok(ctx) if !ctx.is_empty() => self_improve::format_improvement_context(&ctx),
        Ok(_) => String::new(),
        Err(e) => {
            warn!("Failed to analyze generation patterns: {}", e);
            String::new()
        }
    };

    // 2. Similar workflows (needs embedding)
    let similar_section = if let Some(emb) = query_embedding {
        match similar_workflows::find_similar_workflows(conn, emb, category, 3) {
            Ok(similar) if !similar.is_empty() => {
                similar_workflows::format_similar_workflows(&similar)
            }
            Ok(_) => String::new(),
            Err(e) => {
                warn!("Failed to find similar workflows: {}", e);
                String::new()
            }
        }
    } else {
        String::new()
    };

    // 2b. Ground truth reference workflows (keyword + optional embedding)
    let gt_reference_section =
        match similar_workflows::find_gt_reference_workflows(conn, description, query_embedding, 2)
        {
            Ok(gt) if !gt.is_empty() => {
                info!("Found {} GT reference workflows", gt.len());
                similar_workflows::format_gt_references(&gt)
            }
            Ok(_) => String::new(),
            Err(e) => {
                warn!("Failed to find GT reference workflows: {}", e);
                String::new()
            }
        };

    // 3. Verifier focus items (top issues from self-improvement data)
    let verifier_focus_items = match self_improve::analyze_generation_patterns(conn) {
        Ok(ctx) => ctx
            .common_verifier_issues
            .iter()
            .take(5)
            .map(|(issue, count)| format!("{} (seen {} times)", issue, count))
            .collect(),
        Err(_) => Vec::new(),
    };

    // 4. Past successful fixes
    let past_fixes_section = build_past_fixes_section(conn);

    if improvement_section.is_empty()
        && similar_section.is_empty()
        && gt_reference_section.is_empty()
        && verifier_focus_items.is_empty()
        && past_fixes_section.is_empty()
    {
        return None;
    }

    Some(HistoricalContext {
        improvement_section,
        similar_section,
        verifier_focus_items,
        past_fixes_section,
        gt_reference_section,
    })
}

/// Build a meta-workflow that generates a workflow from a natural language description.
///
/// The meta-workflow has 4 phases:
/// - **Setup**: Builder agent generates initial workflow JSON (prompt in response mode)
/// - **Verification**: AI review checks semantic correctness of the generated workflow
/// - **Agentic**: Fixer agent corrects issues found by verification (prompt in response mode)
/// - **Completion**: Saves the generated workflow to the library
///
/// # Arguments
/// * `request` - The original workflow generation request with description, options, etc.
/// * `resolved_contexts` - Pre-resolved context text to inject into the builder prompt.
/// * `historical_context` - Optional historical context for enhanced prompts.
pub fn build_meta_workflow_template(
    request: &GenerateWorkflowRequest,
    resolved_contexts: &str,
    historical_context: Option<&HistoricalContext>,
) -> UnifiedWorkflow {
    let now = chrono::Utc::now().to_rfc3339();
    let max_fix_iterations = request.max_fix_iterations.unwrap_or(3);

    // Truncate description for the workflow name
    let name_suffix = if request.description.len() > 50 {
        format!("{}...", &request.description[..50])
    } else {
        request.description.clone()
    };

    // Build the prompt strings for each agent
    let schema_context = build_schema_context_for_description(&request.description);
    let is_plan_mode = request
        .generation_mode
        .as_deref()
        .map(|m| m == "plan")
        .unwrap_or(false);
    let builder_prompt = if is_plan_mode {
        build_plan_builder_prompt(
            &request.description,
            &schema_context,
            resolved_contexts,
            request.prompt_template.as_deref(),
            request.category.as_deref(),
            historical_context,
        )
    } else {
        build_builder_prompt(
            &request.description,
            &schema_context,
            resolved_contexts,
            request.prompt_template.as_deref(),
            request.category.as_deref(),
            historical_context,
        )
    };
    let verification_prompt = build_verification_review_prompt(historical_context);
    let fixer_prompt =
        build_fixer_base_prompt(&request.description, &schema_context, historical_context);

    UnifiedWorkflow {
        id: Uuid::new_v4().to_string(),
        name: format!("AI Generate: {}", name_suffix),
        description: format!(
            "Meta-workflow: generates a workflow from description: {}",
            request.description
        ),
        category: "meta".to_string(),
        tags: vec!["meta".to_string(), "generation".to_string()],

        // Phase 1: Builder agent generates initial workflow JSON
        setup_steps: vec![json!({
            "id": Uuid::new_v4().to_string(),
            "name": "Generate workflow from description",
            "type": "prompt",
            "phase": "setup",
            "prompt_mode": "response",
            "content": builder_prompt,
            "output_path": "{{artifact_dir}}/workflow.json"
        })],

        // Phase 2: AI semantic review of the generated workflow
        verification_steps: vec![json!({
            "id": Uuid::new_v4().to_string(),
            "name": "AI semantic review",
            "type": "check",
            "phase": "verification",
            "check_type": "ai_review",
            "ai_review_prompt": verification_prompt,
            "ai_review_input_path": "{{artifact_dir}}/workflow.json",
            "ai_review_validate_as_workflow": true
        })],

        // Phase 3: Fixer agent corrects issues found by verification
        agentic_steps: vec![json!({
            "id": Uuid::new_v4().to_string(),
            "name": "Fix verification issues",
            "type": "prompt",
            "phase": "agentic",
            "prompt_mode": "response",
            "content": fixer_prompt,
            "input_path": "{{artifact_dir}}/workflow.json",
            "output_path": "{{artifact_dir}}/workflow.json"
        })],

        // Phase 4: Hardening + Self-analysis (dev only) + Save the generated workflow
        completion_steps: vec![
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Harden prompt verification steps",
                "type": "prompt",
                "phase": "completion",
                "prompt_mode": "response",
                "content": build_hardener_completion_prompt(),
                "input_path": "{{artifact_dir}}/workflow.json",
                "output_path": "{{artifact_dir}}/workflow.json"
            }),
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Analyze generation quality (dev)",
                "type": "prompt",
                "phase": "completion",
                "prompt_mode": "response",
                "dev_mode_only": true,
                "content": "Review the workflow generation that just completed. Compare the generated workflow with the original description. Identify: (1) aspects the builder got right, (2) issues the verifier caught, (3) issues the verifier missed, (4) patterns that could improve future generations. Output as [FINDING:...] markers.",
                "input_path": "{{artifact_dir}}/workflow.json"
            }),
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Save generated workflow",
                "type": "save_workflow_artifact",
                "phase": "completion",
                "artifact_input_path": "{{artifact_dir}}/workflow.json"
            }),
        ],

        max_iterations: max_fix_iterations,
        timeout_seconds: None,
        provider: request.provider.clone(),
        model: request.model.clone(),
        skip_ai_summary: request.skip_ai_summary.unwrap_or(true),
        targeted_error_ids: Vec::new(),
        log_source_selection: LogSourceSelection::default(),
        context_ids: request.context_ids.clone().unwrap_or_default(),
        disabled_context_ids: Vec::new(),
        auto_include_contexts: request.auto_include_contexts.unwrap_or(false),
        prompt_template: None,
        log_watch_enabled: false,
        health_check_enabled: false,
        health_check_urls: Vec::new(),
        preflight_check_enabled: false,
        enable_sweep: false,
        max_sweep_iterations: 5,
        generated_by_task_run_id: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

// ============================================================================
// Prompt builders (private)
// ============================================================================

/// Build the prompt for the builder agent that generates the initial workflow JSON.
fn build_builder_prompt(
    description: &str,
    schema_context: &str,
    resolved_contexts: &str,
    custom_template: Option<&str>,
    category: Option<&str>,
    historical: Option<&HistoricalContext>,
) -> String {
    let mut prompt = format!(
        r#"You are a workflow generation AI. Your task is to create a UnifiedWorkflow JSON from a natural language description.

## Workflow Schema & Rules

{schema_context}

## Task Description

{description}
"#,
        schema_context = schema_context,
        description = description,
    );

    // Add category hint if provided
    if let Some(cat) = category {
        prompt.push_str(&format!("\nUse category: {}\n", cat));
    }

    // Add resolved contexts if any
    if !resolved_contexts.is_empty() {
        prompt.push_str(&format!(
            "\n## Additional Context\n\n{}\n",
            resolved_contexts
        ));
    }

    // Add custom template if provided
    if let Some(template) = custom_template {
        prompt.push_str(&format!("\n## Custom Instructions\n\n{}\n", template));
    }

    // === ENHANCED: Add historical patterns, similar workflows, and GT references ===
    if let Some(ctx) = historical {
        // GT references go first (highest priority — verified correct examples)
        if !ctx.gt_reference_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.gt_reference_section);
        }
        if !ctx.improvement_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.improvement_section);
        }
        if !ctx.similar_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.similar_section);
        }
    }

    prompt.push_str(
        r#"
## Output Requirements

1. Output ONLY valid JSON — no markdown, no code fences, no explanation
2. The JSON must be a complete UnifiedWorkflow object
3. Every step must have a unique UUID v4 `id` field
4. Every step's `phase` field must match the array it's in
5. Use descriptive `name` fields for each step
6. Set reasonable `timeout_seconds` for each step
7. The `category` should be appropriate for the task (e.g., "testing", "deployment", "monitoring")
8. Include meaningful `description` and `tags`

### Quality checklist — ensure your output meets ALL of these:
- Every `shell_command` has a real, syntactically-valid command (no placeholders)
- Every `check` step has a `command` that matches its `check_type` (e.g. lint -> eslint/ruff, typecheck -> tsc/mypy)
- Every `api_request` has a well-formed URL starting with http:// or https://
- Every `test` step has a valid `test_type` and either a `command` or `code` field
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions referencing verification results
- If verification steps exist there MUST be at least one agentic `prompt` step
- `gate` steps only reference `required_steps` IDs that exist in the same phase
- Step names are descriptive (not "Step 1", "Test", etc.)
- `working_directory` paths are real absolute or project-relative paths (no placeholders)

### CRITICAL — Verification must be automated, not just AI judgment:
- verification_steps MUST include at least one automated step (`check`, `test`, `spec`, or `api_request` with assertions). NEVER create a workflow where ALL verification steps are `prompt` type — this is the #1 most common generation mistake.
- If the workflow creates/modifies TypeScript files, ALWAYS include: `{{"type": "check", "check_type": "typecheck", "command": "npx tsc --noEmit", "working_directory": "<project_dir>"}}`
- If the workflow creates/modifies Python files, ALWAYS include: `{{"type": "check", "check_type": "typecheck", "command": "mypy .", "working_directory": "<project_dir>"}}`
- If the workflow targets a web app (localhost:3001 or localhost:1420), ALWAYS include a UI Bridge SDK `api_request` step in verification to verify UI state — e.g., `GET /ui-bridge/sdk/elements` with assertions. Add SDK connect in setup first.
- Include a `gate` step that depends on all automated check/test/spec steps
- `prompt` steps in verification are fine as SUPPLEMENTARY checks but must not be the only verification

### UI Bridge SDK Capabilities & Limitations

When the workflow uses the UI Bridge SDK for verification, choose the right tool for each check:

**SDK CAN do (use `api_request` steps with SDK endpoints):**
- Click, double-click, right-click elements
- Type text into inputs
- Select dropdown options
- Focus/blur elements
- Hover over elements
- Scroll elements
- Check/uncheck/toggle checkboxes
- Drag-and-drop between elements (pass `target.elementId` or `targetPosition` in params)
- Find and query elements by ID, role, text, or CSS selector
- Get element state (value, checked, disabled, visible, etc.)
- Take page snapshots
- AI-powered semantic search and natural language actions

**SDK CANNOT do (use `test` steps with Playwright instead):**
- Keyboard shortcuts (Ctrl+Z, Delete, Ctrl+S, etc.)
- File upload dialogs
- Form submit/reset actions
- Multi-tab or multi-window interactions
- Screenshot pixel-comparison testing
- Browser devtools or network interception

**Rule:** Use Playwright `test` steps ONLY for interactions the SDK cannot handle. For everything else, prefer SDK `api_request` steps — they are faster, more reliable, and don't require browser binaries.

### Agentic-Verification Coverage

Every agentic step MUST have at least one corresponding verification step that would FAIL if that agentic step's work was NOT done. Common gaps to avoid:
- Agentic step "add keyboard shortcuts" with no verification for keyboard functionality → add a Playwright test
- Agentic step "add thumbnails to nodes" with only a tab-existence check → add an SDK element check for thumbnail content
- Agentic step "implement drag-and-drop" with only a visual review → add a Playwright interaction test

If an agentic step has multiple distinct goals (e.g., "edge labels, initial state badge, and keyboard shortcuts"), each goal needs its own verification step or a combined test that checks all of them.

### Out-of-Scope Enforcement

If the task description includes "out of scope", "do not change", "do not modify", "must not", or similar boundary constraints:
1. Add a verification `check` step with `custom_command` that validates the constraint (e.g., `git diff --name-only | grep -v <allowed-patterns>` to detect changes to files that should not be modified)
2. Or add a `prompt` verification step that specifically reviews whether scope boundaries were respected
3. Reference the specific constraints in the verification step name (e.g., "Verify data model unchanged")

Generate the workflow JSON now:
"#,
    );

    prompt
}

/// Build the prompt for the plan-aware builder agent.
///
/// This variant parses the description as an ordered implementation plan and generates
/// a workflow with per-phase verification steps and plan-aware agentic prompts.
fn build_plan_builder_prompt(
    description: &str,
    schema_context: &str,
    resolved_contexts: &str,
    custom_template: Option<&str>,
    category: Option<&str>,
    historical: Option<&HistoricalContext>,
) -> String {
    let mut prompt = format!(
        r#"You are a workflow generation AI operating in **Plan Mode**. Your task is to create a UnifiedWorkflow JSON from an ordered implementation plan.

## Workflow Schema & Rules

{schema_context}

## Implementation Plan

The following is an ordered implementation plan. Each numbered item or section represents a distinct phase of work that must be completed in sequence.

{description}
"#,
        schema_context = schema_context,
        description = description,
    );

    if let Some(cat) = category {
        prompt.push_str(&format!("\nUse category: {}\n", cat));
    }

    if !resolved_contexts.is_empty() {
        prompt.push_str(&format!(
            "\n## Additional Context\n\n{}\n",
            resolved_contexts
        ));
    }

    if let Some(template) = custom_template {
        prompt.push_str(&format!("\n## Custom Instructions\n\n{}\n", template));
    }

    if let Some(ctx) = historical {
        if !ctx.gt_reference_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.gt_reference_section);
        }
        if !ctx.improvement_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.improvement_section);
        }
        if !ctx.similar_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.similar_section);
        }
    }

    prompt.push_str(
        r#"
## Plan Mode Instructions

Because the input is a structured plan (not a freeform description), follow these additional rules:

### 1. Parse Plan Phases
- Identify each distinct phase/step/section in the plan
- Preserve the ordering — earlier phases should be completed before later ones
- Each plan phase maps to work the AI must do in the agentic phase

### 2. Create Per-Phase Verification Steps
For EACH phase identified in the plan, create verification steps that would FAIL if that phase's work is incomplete:
- Use `check` steps with `custom_command` to verify file changes, compilation, test passes
- Use `shell_command` steps to verify expected outputs exist
- Use `test` steps for automated testing of the phase's deliverables
- Each verification step name should reference the plan phase (e.g., "Verify Phase 1: Schema updates")
- NEVER rely solely on `prompt` type verification — always include automated checks

### 3. Create Plan-Aware Agentic Prompts
The agentic `prompt` step(s) should:
- Reference the full plan with phase numbering
- Instruct the AI to check verification failures and fix the specific failing phase
- Include `[INJECT_STEP]` awareness: mention that the AI can output `[INJECT_STEP]` markers to dynamically add verification steps during execution
- Structure the prompt so the AI works through plan phases systematically

### 4. Gate Steps
- Add a `gate` step that depends on ALL phase-specific verification steps
- This ensures the agentic loop continues until ALL plan phases pass verification

## Output Requirements

1. Output ONLY valid JSON — no markdown, no code fences, no explanation
2. The JSON must be a complete UnifiedWorkflow object
3. Every step must have a unique UUID v4 `id` field
4. Every step's `phase` field must match the array it's in
5. Use descriptive `name` fields for each step
6. Set reasonable `timeout_seconds` for each step

### Quality checklist:
- Every `shell_command` has a real, syntactically-valid command (no placeholders)
- Every `check` step has a `command` that matches its `check_type`
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions referencing the plan phases and verification failures
- verification_steps MUST include at least one automated step per plan phase
- A `gate` step depends on all automated verification step IDs
- Step names reference specific plan phases (not generic names)
- `working_directory` paths are real absolute or project-relative paths

Generate the workflow JSON now:
"#,
    );

    prompt
}

/// Build the prompt for the verification agent that reviews generated workflow JSON.
fn build_verification_review_prompt(historical: Option<&HistoricalContext>) -> String {
    let mut prompt = r#"You are a workflow verification AI. Review the following workflow JSON for correctness.

Check for these issues:
1. **Structural**: Valid JSON, all required fields present, correct types
2. **Step IDs**: All steps have unique UUID v4 IDs
3. **Phase consistency**: Each step's `phase` field matches the array it's in
4. **Step type validity**: Step types are valid and appropriate for their phase
5. **Logical flow**: Steps make logical sense in sequence
6. **Completeness**: The workflow achieves what its description says
7. **Timeouts**: Reasonable timeout values for each step type
8. **Names**: Descriptive, non-empty step names
9. **Command validity**: shell_command steps have real commands (not placeholders)
10. **Check consistency**: check_type and command match (lint -> linter, typecheck -> type checker)
11. **Prompt quality**: Agentic prompts are substantive with specific instructions
12. **Cross-references**: gate required_steps reference existing step IDs
13. **CRITICAL — Automated verification required**: verification_steps MUST include at least one deterministic automated step (check, test, spec, or api_request with assertions). A workflow with ONLY prompt-type verification steps is INVALID — this MUST be flagged as a failure.
14. **Code change typecheck**: If the workflow creates or modifies code files (TypeScript, Python, Rust), there MUST be a `check` step with appropriate typecheck command (npx tsc --noEmit, mypy, cargo check). Missing typecheck for code-modifying workflows MUST be flagged.
15. **Web app SDK verification**: If the workflow targets a web app (localhost:3001, localhost:1420), there SHOULD be a UI Bridge SDK api_request step or spec step in verification to verify UI state programmatically. Flag as a warning if missing.
16. **Gate step**: Workflows with 2+ verification steps SHOULD have a gate step aggregating the automated checks. Flag as a warning if missing.
17. **Agentic-verification coverage**: Every agentic step MUST have at least one verification step that would FAIL if that agentic step's work was NOT done. A tab-existence check does NOT count as coverage for an agentic step that adds content within that tab. Flag as a failure if any agentic step has no corresponding verification.
18. **Out-of-scope enforcement**: If the task description contains "out of scope", "do not change", "do not modify", or similar boundary constraints, there SHOULD be at least one verification step that checks those boundaries were respected. Flag as a warning if missing.
"#
    .to_string();

    // === ENHANCED: Add known problem areas from history ===
    if let Some(ctx) = historical {
        if !ctx.verifier_focus_items.is_empty() {
            prompt.push_str("\n### Pay Special Attention To:\n\n");
            prompt.push_str(
                "These are the most common issues found in previously generated workflows:\n\n",
            );
            for item in &ctx.verifier_focus_items {
                prompt.push_str(&format!("- {}\n", item));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str(
        r#"
If all checks pass, respond with:
{"passed": true, "issues": []}

If there are issues, respond with:
{"passed": false, "issues": ["Issue 1 description", "Issue 2 description"]}

Be strict but fair. Focus on real problems, not style preferences."#,
    );

    prompt
}

/// Build the base prompt for the fixer agent that corrects verification issues.
fn build_fixer_base_prompt(
    description: &str,
    schema_context: &str,
    historical: Option<&HistoricalContext>,
) -> String {
    let mut prompt = format!(
        r#"You are a workflow fixer AI. You will receive a workflow JSON that has verification issues.
Fix ALL the issues while preserving the workflow's intent.

## Schema Reference

{schema_context}

## Original Task Description

{description}
"#,
        schema_context = schema_context,
        description = description,
    );

    // === ENHANCED: Add past successful fixes ===
    if let Some(ctx) = historical {
        if !ctx.past_fixes_section.is_empty() {
            prompt.push('\n');
            prompt.push_str(&ctx.past_fixes_section);
        }
    }

    prompt.push_str(
        r#"
## Instructions

1. Read the input workflow JSON carefully
2. The verification issues will be provided in the failure context
3. Fix each issue while maintaining the overall workflow structure
4. Output ONLY the corrected workflow JSON — no markdown, no code fences, no explanation
5. Preserve all existing step IDs unless they are invalid
6. Do not remove steps unless they are fundamentally broken
7. All UUIDs must be valid v4 format
8. All `phase` fields must match the array they are in

Output the corrected workflow JSON now:
"#,
    );

    prompt
}

/// Build the prompt for the hardener completion step.
///
/// This step reads the generated workflow JSON and converts prompt-type verification
/// steps to deterministic equivalents where possible.
fn build_hardener_completion_prompt() -> String {
    r#"You are a verification hardener agent. Read the input workflow JSON and strengthen verification steps to be as deterministic as possible.

## Rule 1: Convert prompt verification steps to deterministic equivalents

| Prompt check type | Convert to | Method |
|---|---|---|
| UI element presence/structure | `api_request` | UI Bridge SDK `GET /ui-bridge/sdk/elements` with `body_contains` assertion |
| Content/text on page | `api_request` | UI Bridge SDK `POST /ui-bridge/sdk/ai/search` with `body_contains` assertion |
| File existence | `check` | `custom_command` with `test -f <path>` |
| File content | `check` | `custom_command` with `grep -q <pattern> <file>` |
| Code quality (lint) | `check` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `check` | `check_type: "typecheck"` with appropriate command |
| API health/response | `api_request` | Direct HTTP with `status_code` assertion |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |

## Rule 2: Convert Playwright UI tests to UI Bridge SDK api_request steps

If the workflow has a UI Bridge SDK connect step in setup AND has Playwright test steps that verify UI features (element presence, tab existence, visual layout, etc.), convert them to one or more `api_request` steps using the UI Bridge SDK.

- A single Playwright test that checks multiple things SHOULD be split into multiple `api_request` steps
- Keep the original step ID on the first converted step; generate new UUIDs for split steps
- Use SDK endpoints: `GET /ui-bridge/sdk/elements?contentOnly=true` for element checks, `POST /ui-bridge/sdk/ai/search` for semantic search
- Include `body_contains` assertions that check for specific text/patterns

Do NOT convert Playwright tests that are purely functional (form submission, navigation flows) — only convert UI verification tests.

**SDK capability limits:** Do NOT convert Playwright tests involving keyboard shortcuts (Ctrl+Z, Delete, etc.), file uploads, form submit/reset, multi-tab interactions, or screenshot pixel comparisons — the SDK cannot perform these. These MUST stay as Playwright tests. Note: drag-and-drop CAN be converted to SDK — use the `drag` action with `target.elementId` or `targetPosition` params.

## Rule 3: Strengthen weak SDK assertions

If an existing `api_request` step targets a UI Bridge SDK endpoint but only has a `status_code` assertion, add `body_contains` assertions that check for meaningful content based on the step name/description.

Example: A step named "Verify state nodes render thumbnails" hitting `/ui-bridge/sdk/elements` with only `{"type":"status_code","expected":200}` should also assert `{"type":"body_contains","expected":"thumbnail"}` or similar.

## General Rules

1. ONLY modify verification_steps — do NOT change setup_steps, agentic_steps, or completion_steps
2. Preserve all original step IDs (splitting a step keeps the original ID on the first part)
3. Step count may increase (from splitting) but must never decrease
4. If a prompt step is genuinely subjective, keep it as `prompt`
5. For `api_request` conversions, include `assertions` array with at least `status_code` + `body_contains`
6. For `check` conversions, include `check_type`, `command`, and `working_directory`

If there are no steps that need hardening, return the workflow unchanged.

Output ONLY the complete, valid UnifiedWorkflow JSON. No markdown, no code fences, no explanations."#.to_string()
}

/// Build the "past fixes" section from resolved findings on meta task runs.
fn build_past_fixes_section(conn: &Connection) -> String {
    let sql = r#"
        SELECT f.title, f.description, f.resolution
        FROM task_run_findings f
        JOIN task_runs tr ON tr.id = f.task_run_id
        WHERE tr.workflow_name LIKE 'AI Generate:%'
          AND f.status = 'resolved'
          AND f.resolution IS NOT NULL
        ORDER BY f.resolved_at DESC
        LIMIT 5
    "#;

    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    let fixes: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    if fixes.is_empty() {
        return String::new();
    }

    let mut section = String::from("## Fixes That Worked Before\n\n");
    section.push_str(
        "These are previously resolved verification issues. Use them as guidance for similar problems:\n\n",
    );

    for (title, _description, resolution) in &fixes {
        section.push_str(&format!("- **{}**: {}\n", title, resolution));
    }
    section.push('\n');

    section
}
