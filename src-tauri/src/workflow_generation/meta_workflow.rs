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

    // Extract a clean, descriptive name from the description.
    // Strip markdown formatting and grab the first meaningful line.
    let name_suffix = extract_workflow_name_from_description(&request.description);

    // Build the prompt strings for each agent
    let schema_context = build_schema_context_for_description(&request.description);
    let builder_prompt = build_builder_prompt(
        &request.description,
        &schema_context,
        resolved_contexts,
        request.prompt_template.as_deref(),
        request.category.as_deref(),
        historical_context,
    );
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

        // Phase 1: Optional investigation + Builder agent generates initial workflow JSON
        setup_steps: {
            let mut steps = Vec::new();

            if request.investigate_codebase.unwrap_or(true) {
                let investigation_prompt = build_investigation_setup_prompt(&request.description);
                steps.push(json!({
                    "id": Uuid::new_v4().to_string(),
                    "name": "Investigate codebase for generation context",
                    "type": "prompt",
                    "phase": "setup",
                    "prompt_mode": "response",
                    "content": investigation_prompt,
                    "output_path": "{{artifact_dir}}/investigation.md"
                }));
            }

            steps.push(json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Generate workflow from description",
                "type": "prompt",
                "phase": "setup",
                "prompt_mode": "response",
                "content": builder_prompt,
                "input_path": if request.investigate_codebase.unwrap_or(true) {
                    "{{artifact_dir}}/investigation.md"
                } else {
                    ""
                },
                "output_path": "{{artifact_dir}}/workflow.json"
            }));

            steps
        },

        // Phase 2: AI semantic review of the generated workflow
        verification_steps: vec![json!({
            "id": Uuid::new_v4().to_string(),
            "name": "AI semantic review",
            "type": "command",
            "phase": "verification",
            "mode": "check",
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
        stages: Vec::new(),
        stop_on_failure: false,
        reflection_mode: false,
        created_at: now.clone(),
        updated_at: now,
    }
}

// ============================================================================
// Name extraction helpers
// ============================================================================

/// Extract a clean, descriptive workflow name from a potentially long description.
///
/// Handles common cases:
/// - Markdown headings (`# Title` → `Title`)
/// - Multi-line specs (uses first meaningful line)
/// - Long descriptions (truncates to ~60 chars at a word boundary)
fn extract_workflow_name_from_description(description: &str) -> String {
    let max_len = 60;

    // Find the first non-empty, meaningful line
    let first_line = description
        .lines()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .unwrap_or(description.trim());

    // Strip markdown heading markers (e.g., "## My Title" → "My Title")
    let cleaned = first_line.trim_start_matches('#').trim_start();

    // If the first line is very short and looks like a title, and there's a second
    // meaningful line, combine them for more context
    let name = if cleaned.len() < 20 {
        // Look for a second meaningful line to append
        let second_line = description
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .nth(1)
            .map(|l| l.trim_start_matches('#').trim_start())
            .unwrap_or("");

        if !second_line.is_empty() && !second_line.starts_with("```") {
            format!("{} — {}", cleaned, second_line)
        } else {
            cleaned.to_string()
        }
    } else {
        cleaned.to_string()
    };

    // Truncate at a word boundary if too long
    if name.len() <= max_len {
        name
    } else {
        // Find the last space before the limit
        match name[..max_len].rfind(' ') {
            Some(pos) => format!("{}...", &name[..pos]),
            None => format!("{}...", &name[..max_len]),
        }
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
- Every `command` step has a real, syntactically-valid command (no placeholders)
- Every `command` step with `check_type` has a matching command (e.g. lint -> eslint/ruff, typecheck -> tsc/mypy)
- Every `test` step has a valid `test_type` and either a `command` or `code` field
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions referencing verification results
- If verification steps exist there MUST be at least one agentic `prompt` step
- All `depends_on` and `inputs` references point to valid step IDs within the workflow
- No circular dependencies exist in `depends_on` chains
- Step names are descriptive (not "Step 1", "Test", etc.)
- `working_directory` paths are real absolute or project-relative paths (no placeholders)
- Only use these 4 step types: `command`, `test`, `ui_bridge`, `prompt`

### CRITICAL — Verification must be automated, not just AI judgment:
- verification_steps MUST include at least one automated step (`command`, `test`, or `ui_bridge` with assert action). NEVER create a workflow where ALL verification steps are `prompt` type — this is the #1 most common generation mistake.
- If the workflow creates/modifies TypeScript files, ALWAYS include: `{{"type": "command", "check_type": "typecheck", "command": "npx tsc --noEmit", "working_directory": "<project_dir>"}}`
- If the workflow creates/modifies Python files, ALWAYS include: `{{"type": "command", "check_type": "typecheck", "command": "mypy .", "working_directory": "<project_dir>"}}`
- If the workflow targets a web app (localhost:3001 or localhost:1420), ALWAYS include a `ui_bridge` step in verification to verify UI state. Add SDK connect in setup first.
- `prompt` steps in verification are fine as SUPPLEMENTARY checks but must not be the only verification

### Data Flow Between Steps
- Use `extract` to capture output values from a step (e.g., `{{"token": "$.access_token"}}`)
- Use `inputs` on downstream steps to inject extracted values (e.g., `{{"auth_token": "${{login_step.token}}"}}`)
- Use `depends_on` to enforce execution order when data dependencies exist
- Mark non-critical steps with `"required": false` so their failure does not block the workflow

### UI Bridge SDK Capabilities & Limitations

When the workflow uses the UI Bridge SDK for verification, choose the right tool for each check:

**SDK CAN do (use `command` steps with curl or `ui_bridge` steps):**
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

### CRITICAL PROHIBITIONS

You MUST NOT do any of the following:
- Make code changes, create files, edit files, or modify any project code
- Execute shell commands or terminal operations
- Ask clarifying questions or request more information
- Provide explanations, commentary, or markdown formatting
- Output anything other than a single JSON object
- Interpret the task description as instructions for YOU to execute

You are a workflow DESIGNER, not a workflow EXECUTOR. Your only output is a workflow JSON specification.
If the task description is ambiguous, interpret it as a workflow generation request and make reasonable assumptions.

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
4. **Step type validity**: Step types must be one of the 4 core types (command, test, ui_bridge, prompt) and appropriate for their phase
5. **Logical flow**: Steps make logical sense in sequence
6. **Completeness**: The workflow achieves what its description says
7. **Timeouts**: Reasonable timeout values for each step type
8. **Names**: Descriptive, non-empty step names
9. **Command validity**: command steps have real commands (not placeholders)
10. **Check consistency**: check_type and command match (lint -> linter, typecheck -> type checker)
11. **Prompt quality**: Agentic prompts are substantive with specific instructions
12. **Data flow validity**: All `inputs` and `depends_on` references point to existing step IDs. No circular dependencies in `depends_on` chains. `extract` expressions are valid.
13. **CRITICAL — Automated verification required**: verification_steps MUST include at least one deterministic automated step (command, test, or ui_bridge with assert action). A workflow with ONLY prompt-type verification steps is INVALID — this MUST be flagged as a failure.
14. **Code change typecheck**: If the workflow creates or modifies code files (TypeScript, Python, Rust), there MUST be a `command` step with `check_type: "typecheck"` and appropriate command (npx tsc --noEmit, mypy, cargo check). Missing typecheck for code-modifying workflows MUST be flagged.
15. **Web app SDK verification**: If the workflow targets a web app (localhost:3001, localhost:1420), there SHOULD be a `ui_bridge` step in verification to verify UI state programmatically. Flag as a warning if missing.
16. **Agentic-verification coverage**: Every agentic step MUST have at least one verification step that would FAIL if that agentic step's work was NOT done. A tab-existence check does NOT count as coverage for an agentic step that adds content within that tab. Flag as a failure if any agentic step has no corresponding verification.
17. **Out-of-scope enforcement**: If the task description contains "out of scope", "do not change", "do not modify", or similar boundary constraints, there SHOULD be at least one verification step that checks those boundaries were respected. Flag as a warning if missing.
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

Determine which mode to use based on the input:

### Mode 1 — Fix (input contains workflow JSON)

If the input contains a JSON workflow object, fix it:

1. Read the input workflow JSON carefully
2. The verification issues will be provided in the failure context
3. Fix each issue while maintaining the overall workflow structure
4. Output ONLY the corrected workflow JSON — no markdown, no code fences, no explanation
5. Preserve all existing step IDs unless they are invalid
6. Do not remove steps unless they are fundamentally broken
7. All UUIDs must be valid v4 format
8. All `phase` fields must match the array they are in
9. Ensure all `inputs` and `depends_on` references point to valid step IDs
10. Ensure no circular dependencies exist in `depends_on` chains
11. Only use the 4 core step types: command, test, ui_bridge, prompt

### Mode 2 — Generate from scratch (input is empty or not valid JSON)

If the input is empty, contains non-JSON text, or contains code changes instead of workflow JSON:

1. IGNORE the input entirely — it is the result of a failed generation attempt
2. Read the **Original Task Description** above carefully
3. Generate a complete, valid workflow JSON from scratch based on that description
4. Follow the same schema and quality standards as the original builder
5. Include all 4 phases: setup_steps, verification_steps, agentic_steps, completion_steps
6. Ensure at least one automated verification step (not just prompts)
7. Output ONLY the workflow JSON — no markdown, no code fences, no explanation

Output the workflow JSON now:
"#,
    );

    prompt
}

/// Build the prompt for the investigation setup step in the meta-workflow.
///
/// This prompt instructs the AI to analyze the codebase in context of the
/// user's task and produce an enriched description for the builder agent.
fn build_investigation_setup_prompt(description: &str) -> String {
    format!(
        r#"You are a codebase investigation AI. Your job is to analyze the user's task description in context of the project and produce an enriched, technically-grounded version.

## User's Original Task Description

{description}

## Your Task

Analyze the user's intent in context of the project structure (from discovery context) and produce an **enriched task description**:

1. **Preserve the original intent** — do not change what the user wants to accomplish
2. **Identify relevant components** — name specific files, directories, modules, and patterns relevant to the task
3. **Note technical context** — mention frameworks, build tools, test runners, and conventions
4. **Flag potential issues** — dead code paths, missing implementations, broken data flows
5. **Specify concrete targets** — replace vague references with specific file paths and component names
6. **Mention verification approaches** — suggest what should be checked to verify the work

Output ONLY the enriched task description as plain text. No JSON, no code blocks, no prefixes."#,
        description = description,
    )
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
| UI element presence/structure | `ui_bridge` | `action: "assert"` with `assert_type: "element_exists"` and target selector |
| Content/text on page | `ui_bridge` | `action: "assert"` with `assert_type: "element_text"` and expected value |
| File existence | `command` | `command` with `test -f <path>` |
| File content | `command` | `command` with `grep -q <pattern> <file>` |
| Code quality (lint) | `command` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `command` | `check_type: "typecheck"` with appropriate command |
| API health/response | `command` | `command` with `curl` and exit code check |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |

## Rule 2: Convert Playwright UI tests to ui_bridge steps

If the workflow has Playwright test steps that verify UI features (element presence, tab existence, visual layout, etc.), convert them to `ui_bridge` steps with `action: "assert"`.

- A single Playwright test that checks multiple things SHOULD be split into multiple `ui_bridge` assert steps
- Keep the original step ID on the first converted step; generate new UUIDs for split steps
- Use `assert_type` values: `element_exists`, `element_text`, `element_visible`, `page_title`, `element_count`

Do NOT convert Playwright tests that are purely functional (form submission, navigation flows) — only convert UI verification tests.

**SDK capability limits:** Do NOT convert Playwright tests involving keyboard shortcuts (Ctrl+Z, Delete, etc.), file uploads, form submit/reset, multi-tab interactions, or screenshot pixel comparisons — the SDK cannot perform these. These MUST stay as Playwright tests.

## Rule 3: Strengthen weak ui_bridge steps

If an existing `ui_bridge` step with `action: "assert"` has no `expected` value, add an appropriate `expected` value based on the step name/description.

Example: A step named "Verify state nodes render thumbnails" with `assert_type: "element_exists"` should include a meaningful `target` selector and `expected` value.

## General Rules

1. ONLY modify verification_steps — do NOT change setup_steps, agentic_steps, or completion_steps
2. Preserve all original step IDs (splitting a step keeps the original ID on the first part)
3. Step count may increase (from splitting) but must never decrease
4. If a prompt step is genuinely subjective, keep it as `prompt`
5. For `command` conversions, include `command` and `working_directory`; optionally `check_type` for lint/typecheck/test checks
6. Only 4 valid step types: `command`, `test`, `ui_bridge`, `prompt`

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_strips_markdown_heading() {
        let desc = "# UI Bridge Spec Assertion Types\n\nSome detailed spec...";
        let name = extract_workflow_name_from_description(desc);
        assert!(
            !name.starts_with('#'),
            "Name should not start with '#': {}",
            name
        );
        assert!(name.contains("UI Bridge Spec Assertion Types"));
    }

    #[test]
    fn test_extract_name_multi_level_heading() {
        let desc = "### Chat Page Testing\n\nVerify the chat page works correctly";
        let name = extract_workflow_name_from_description(desc);
        assert_eq!(
            name,
            "Chat Page Testing — Verify the chat page works correctly"
        );
    }

    #[test]
    fn test_extract_name_plain_text() {
        let desc = "Test the login flow end-to-end";
        let name = extract_workflow_name_from_description(desc);
        assert_eq!(name, "Test the login flow end-to-end");
    }

    #[test]
    fn test_extract_name_truncates_long_text() {
        let desc = "This is a very long description that goes on and on about all the things that need to be tested in the application";
        let name = extract_workflow_name_from_description(desc);
        assert!(name.len() <= 63); // 60 + "..."
        assert!(name.ends_with("..."));
    }

    #[test]
    fn test_extract_name_short_title_gets_context() {
        let desc = "# Chat Page\n\nVerify all chat functionality works correctly";
        let name = extract_workflow_name_from_description(desc);
        assert!(name.contains("Chat Page"), "Should contain title: {}", name);
        assert!(
            name.contains("Verify"),
            "Should include second line for context: {}",
            name
        );
    }

    #[test]
    fn test_extract_name_skips_empty_lines() {
        let desc = "\n\n  \n# My Workflow\n\nDetails here";
        let name = extract_workflow_name_from_description(desc);
        assert!(name.contains("My Workflow"));
    }
}
