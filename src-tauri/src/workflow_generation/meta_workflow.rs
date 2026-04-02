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

use crate::database::Connection;
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
    None
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
    let is_simple = request.simple_mode == Some(true);
    let max_fix_iterations = if is_simple {
        std::cmp::min(request.max_fix_iterations.unwrap_or(3), 1)
    } else {
        request.max_fix_iterations.unwrap_or(3)
    };

    // Extract a clean, descriptive name from the description.
    // Strip markdown formatting and grab the first meaningful line.
    let name_suffix = extract_workflow_name_from_description(&request.description);

    // Read any referenced files from the description and inject their contents
    let referenced_file_contents = read_referenced_files_from_description(&request.description);

    // Build the specification prompt (acceptance criteria)
    let specification_prompt = build_specification_meta_prompt(&request.description);

    // Build the prompt strings for each agent
    let schema_context = build_schema_context_for_description(&request.description);
    let builder_prompt = build_builder_prompt(
        &request.description,
        &schema_context,
        resolved_contexts,
        request.prompt_template.as_deref(),
        request.category.as_deref(),
        historical_context,
        &referenced_file_contents,
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

        // Phase 1: Investigation → Specification → Builder
        setup_steps: {
            let mut steps = Vec::new();

            // Step 1: Investigation (optional) — enriches task description with codebase context
            if request.investigate_codebase.unwrap_or(true) {
                let investigation_prompt = build_investigation_setup_prompt(
                    &request.description,
                    &referenced_file_contents,
                );
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

            // Step 2: Specification — generates acceptance criteria from task description
            // Non-required: specification enriches quality but the builder can proceed without it.
            // This matches the sync path's graceful degradation (generator.rs uses criteria.unwrap_or(None)).
            if !is_simple {
                let mut spec_step = json!({
                    "id": Uuid::new_v4().to_string(),
                    "name": "Define acceptance criteria",
                    "type": "prompt",
                    "phase": "setup",
                    "prompt_mode": "response",
                    "content": specification_prompt,
                    "output_path": "{{artifact_dir}}/criteria.json",
                    "required": false,
                    "retry_count": 2,
                    "retry_delay_ms": 3000
                });
                if request.investigate_codebase.unwrap_or(true) {
                    spec_step["input_path"] = json!("{{artifact_dir}}/investigation.md");
                }
                steps.push(spec_step);
            }

            // Step 3: Builder — generates workflow JSON, informed by acceptance criteria.
            // input_path reads criteria.json produced by the non-required spec step.
            // If spec failed/skipped, criteria.json won't exist — the builder proceeds
            // without it since input_path reads are now gracefully degraded to warnings.
            // retry_count ensures transient AI provider failures don't kill the entire
            // generation pipeline (the builder step is the critical path).
            {
                let builder_step = json!({
                    "id": Uuid::new_v4().to_string(),
                    "name": "Generate workflow from description",
                    "type": "prompt",
                    "phase": "setup",
                    "prompt_mode": "response",
                    "content": builder_prompt,
                    "output_path": "{{artifact_dir}}/workflow.json",
                    "input_path": "{{artifact_dir}}/criteria.json",
                    "retry_count": 2,
                    "retry_delay_ms": 5000
                });
                steps.push(builder_step);
            }

            steps
        },

        // Phase 2: Deterministic autofix → AI semantic review (with criteria cross-validation)
        verification_steps: vec![
            // Step 1: Deterministic autofix (UUID generation, phase assignment, command sanitization)
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Autofix workflow structure",
                "type": "workflow_fixup",
                "phase": "verification",
                "fixup_input_path": "{{artifact_dir}}/workflow.json",
                "fixup_mode": "autofix"
            }),
            // Step 2: AI semantic review with acceptance criteria cross-validation
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "AI semantic review",
                "type": "command",
                "phase": "verification",
                "mode": "check",
                "check_type": "ai_review",
                "ai_review_prompt": verification_prompt,
                "ai_review_input_path": "{{artifact_dir}}/workflow.json",
                "ai_review_validate_as_workflow": true,
                "ai_review_criteria_path": "{{artifact_dir}}/criteria.json"
            }),
        ],

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

        // Phase 4: AI Hardener (prompt) → Deterministic harden + Save (automation)
        // With completion_prompts_first=true, prompts run before automation, so:
        //   1. AI hardener prompt runs first
        //   2. Then automation: workflow_fixup (harden) → save_workflow_artifact
        completion_steps: vec![
            // Prompt step: AI hardener (runs first due to completion_prompts_first)
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
            // Prompt step: Self-analysis (dev only, runs with hardener)
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
            // Automation step: Deterministic hardening fixups (runs after AI hardener)
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Apply deterministic hardening",
                "type": "workflow_fixup",
                "phase": "completion",
                "fixup_input_path": "{{artifact_dir}}/workflow.json",
                "fixup_mode": "harden"
            }),
            // Automation step: Save to DB + capture PipelineArtifact (runs last)
            json!({
                "id": Uuid::new_v4().to_string(),
                "name": "Save generated workflow",
                "type": "save_workflow_artifact",
                "phase": "completion",
                "artifact_input_path": "{{artifact_dir}}/workflow.json",
                "artifact_capture_prompts": true
            }),
        ],

        max_iterations: if is_simple { 1 } else { max_fix_iterations },
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
        constraint_overrides: std::collections::HashMap::new(),
        approval_gate: false,
        reflection_mode: false,
        completion_prompts_first: true, // AI hardener must run BEFORE save_workflow_artifact
        is_favorite: false,
        dependency_graph: None,
        cost_annotations: None,
        quality_report: None,
        acceptance_criteria: None,
        ai_reviewed: true,
        multi_agent_mode: true,
        strict_cwd: false,
        tool_tags: Vec::new(),
        use_worktree: false,
        model_overrides: std::collections::HashMap::new(),
        enforce_token_budget: false,
        rollback_policy: None,
        workflow_architecture: None,
        flow_control_json: None,
        phase_timeouts_json: None,
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

/// Build the prompt for the specification step in the meta-workflow.
///
/// Adapts the specification logic from `specification.rs` for the file-based
/// meta-workflow pipeline. The AI generates `AcceptanceCriteria` as JSON,
/// which is then read by the builder step.
fn build_specification_meta_prompt(description: &str) -> String {
    // Reuse the same prompt structure as the sync specification agent,
    // but adapted for the meta-workflow context where investigation.md
    // may be provided as input_path content.
    format!(
        r#"You are a specification agent for an automation platform. Your job is to define **acceptance criteria** — concrete, observable conditions that prove a task was completed successfully.

You do NOT implement anything. You only define what "done" looks like.

## Task Description

{description}

## Additional Context

If additional context was provided (investigation results, project structure), use it to inform your criteria. Focus on what can be verified automatically.

## Instructions

Think about what observable success looks like for this task. Focus on outcomes, not implementation.

For each criterion, consider:
- **What can be checked automatically?** Prefer command-based checks (exit codes, grep, test runners) and UI Bridge assertions over manual review.
- **What is the minimum bar?** Critical criteria are necessary conditions. Optional criteria are nice-to-have.
- **What would a reviewer check?** If a human reviewed this work, what would they verify?

Produce 3–8 structured criteria. Each criterion must have:
- `id`: kebab-case identifier (e.g., "typecheck-passes", "button-renders-correctly")
- `description`: one-sentence description of the success condition
- `method`: how to verify it — one of "command", "ui_bridge", "test", "manual"
- `priority`: one of "critical", "important", "optional"
- `verification_hint`: a concrete suggestion for how to check it (e.g., "Run `npx tsc --noEmit`", "Assert element with id 'button-save' is visible via UI Bridge")
- `category`: grouping label (e.g., "compilation", "ui-content", "behavior", "data-integrity", "style")

Also provide:
- `goal_summary`: one sentence summarizing what overall success looks like
- `assumptions`: list of assumptions you're making (e.g., "Project uses TypeScript", "Frontend runs on localhost:3001")

## Output Format

Return ONLY valid JSON matching this structure. No markdown code blocks, no explanations.

{{
  "goal_summary": "...",
  "criteria": [
    {{
      "id": "...",
      "description": "...",
      "method": "command|ui_bridge|test|manual",
      "priority": "critical|important|optional",
      "verification_hint": "...",
      "category": "..."
    }}
  ],
  "assumptions": ["...", "..."]
}}"#,
        description = description,
    )
}

/// Build the prompt for the builder agent that generates the initial workflow JSON.
fn build_builder_prompt(
    description: &str,
    schema_context: &str,
    resolved_contexts: &str,
    custom_template: Option<&str>,
    category: Option<&str>,
    historical: Option<&HistoricalContext>,
    referenced_file_contents: &str,
) -> String {
    let file_section = if referenced_file_contents.is_empty() {
        String::new()
    } else {
        format!(
            r#"

## Referenced File Contents

**The user's description references the file(s) below. These file contents are the task specification. Base the generated workflow primarily on their contents.**

{referenced_file_contents}
"#,
            referenced_file_contents = referenced_file_contents,
        )
    };

    let mut prompt = format!(
        r#"You are a workflow generation AI. Your task is to create a UnifiedWorkflow JSON from a natural language description.

## Workflow Schema & Rules

{schema_context}

## Task Description

{description}
{file_section}"#,
        schema_context = schema_context,
        description = description,
        file_section = file_section,
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

    // Add project context so the AI knows the file tree and root path.
    // Resolve actual values now instead of leaving {{project_root}} / {{project_structure}}
    // as template variables — those are only substituted into base_prompt (agentic phase),
    // not into setup step content, causing the AI to see literal placeholder text.
    {
        let project_root = crate::mcp::shared::current_project_path().unwrap_or_else(|| {
            // Fallback to CWD if workspace root can't be determined
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        });
        let project_tree = crate::reflection::workflow::generate_file_tree(&project_root);
        let tree_section = if project_tree.is_empty() {
            format!("(Could not read directory tree for: {})", project_root)
        } else {
            project_tree
        };
        prompt.push_str(&format!(
            r#"
## Project Context

Project root: {project_root}

### Project Structure
{tree_section}

Use this structure to write accurate file paths in verification commands and working_directory fields.
The generated workflow has FULL filesystem access when executed — it is NOT restricted to any subdirectory.
Commands in the generated workflow can read, write, and execute in any directory within the project.
"#,
            project_root = project_root,
            tree_section = tree_section,
        ));
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

    // Add criteria awareness instructions
    prompt.push_str(
        r#"
## Acceptance Criteria (from input)

The input to this step contains JSON acceptance criteria generated by the specification agent.
These criteria define the observable success conditions for this workflow.

**You MUST:**
- Include a verification step for each automatable criterion (method ≠ "manual")
- Tag each verification step with a `"criterion_id"` field matching the criterion's `id`
- Match the step type to the criterion's method: `command` → command step, `ui_bridge` → ui_bridge step, `test` → command step with test_type
- Ensure at least one verification step per automatable criterion
- Use the `verification_hint` from each criterion to guide your step configuration

"#,
    );

    prompt.push_str(
        r#"
## Output Requirements

1. Output ONLY valid JSON — no markdown, no code fences, no explanation
2. The JSON must be a complete UnifiedWorkflow object
3. Every step must have a unique UUID v4 `id` field
4. Every step's `phase` field must match the array it's in
5. Every step MUST have a descriptive `name` field that explains what it verifies or does (e.g., "Verify WCAG contrast functions exist", "TypeScript compilation check"). Names like "Run grep", "Step 1", or "Test" are NOT acceptable — the name should describe the PURPOSE, not the tool used.
6. Set reasonable `timeout_seconds` for each step
7. The `category` should be appropriate for the task (e.g., "testing", "deployment", "monitoring")
8. Include meaningful `description` and `tags`

### Quality checklist — ensure your output meets ALL of these:
- Do NOT add `npm install`, `pip install`, `poetry install`, or other dependency-install steps — all projects already have dependencies installed
- Every `command` step has a real, syntactically-valid command (no placeholders)
- Every `command` step with `check_type` has a matching command (e.g. lint -> eslint/ruff, typecheck -> tsc/mypy)
- Every `test` step has a valid `test_type` and either a `command` or `code` field
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions referencing verification results
- If verification steps exist there MUST be at least one agentic `prompt` step
- All `depends_on` and `inputs` references point to valid step IDs within the workflow
- No circular dependencies exist in `depends_on` chains
- Step names are descriptive (not "Step 1", "Test", etc.)
- `working_directory` paths are real absolute or project-relative paths (no placeholders)
- Only 3 step types exist: `command`, `ui_bridge`, and `prompt`. Do NOT use `test`, `shell_command`, `api_request`, `check`, `gate`, or `spec`. Tests are run via `command` with `test_type` set. Checks are run via `command` with `check_type` set.

### CRITICAL — Verification must be automated, not just AI judgment:
- verification_steps MUST include at least one automated step (`command` or `ui_bridge` with assert action). NEVER create a workflow where ALL verification steps are `prompt` type — this is the #1 most common generation mistake.
- If the workflow creates/modifies TypeScript files, ALWAYS include: `{{"type": "command", "check_type": "typecheck", "command": "npx tsc --noEmit", "working_directory": "<project_dir>"}}`
- If the workflow creates/modifies Python files, ALWAYS include: `{{"type": "command", "check_type": "typecheck", "command": "mypy .", "working_directory": "<project_dir>"}}`
- If the workflow targets a web app (localhost:3001 or localhost:1420) AND verifies interactive UI behavior (element presence, state, interactions), include a `ui_bridge` step in verification with SDK connect in setup. For simple health checks or content verification (page loads, contains specific text), a `command` step with `curl` is sufficient and preferred.
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

**SDK CAN also do (error monitoring — use `command` steps with curl):**
- Check app health: `curl -s $BASE/sdk/console/health` returns score 0-100 and error breakdown
- Capture error baseline before changes: `curl -s -X POST $BASE/sdk/console/error-baselines/capture -H "Content-Type: application/json" -d '{{"label":"before-fix"}}'`
- Compare after changes: `curl -s -X POST $BASE/sdk/console/error-baselines/compare -H "Content-Type: application/json" -d '{{"label":"before-fix"}}'` — returns new errors (regressions) and fixed errors
- Track errors around an action: start session with `$BASE/sdk/console/error-sessions/start`, perform action, end with `$BASE/sdk/console/error-sessions/end`
- When the workflow modifies UI code, include a console health check in verification to catch runtime errors introduced by the change

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

- When the task involves **removing** a page, route, component, or feature from a web app, verification MUST include a **runtime check** confirming the removal (e.g., UI Bridge navigate to the removed URL and assert 404/redirect, or use sdk/ai/search to verify removed nav elements are absent). Static file-existence checks (`test ! -f`) alone are NOT sufficient — build caches and routing config may still serve removed content.
- In multi-stage workflows, EACH stage that modifies UI behavior (adds, removes, or changes visible pages/components) MUST have its own UI Bridge verification steps. Do not rely on UI Bridge checks in other stages to cover this stage's UI changes.

### Out-of-Scope Enforcement

If the task description includes "out of scope", "do not change", "do not modify", "must not", or similar boundary constraints:
1. Add a verification `check` step with `custom_command` that validates the constraint (e.g., `git diff --name-only | grep -v <allowed-patterns>` to detect changes to files that should not be modified)
2. Or add a `prompt` verification step that specifically reviews whether scope boundaries were respected
3. Reference the specific constraints in the verification step name (e.g., "Verify data model unchanged")

### CRITICAL PROHIBITIONS (for YOU, the design agent)

These prohibitions apply to YOU as the workflow designer. They do NOT apply to the
workflows you generate — the generated workflows WILL be executed with full filesystem
access, shell command execution, and API access across the entire project.

You MUST NOT do any of the following:
- Make code changes, create files, edit files, or modify any project code
- Execute shell commands or terminal operations
- Ask clarifying questions or request more information
- Provide explanations, commentary, or markdown formatting
- Output anything other than a single JSON object
- Interpret the task description as instructions for YOU to execute

You are a workflow DESIGNER, not a workflow EXECUTOR. Your only output is a workflow JSON specification.
The workflows you design will have full access to the project filesystem, shell commands,
UI Bridge SDK endpoints, and all runner APIs when they are executed.
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
4. **Step type validity**: Step types must be one of the 3 core types (command, ui_bridge, prompt) and appropriate for their phase. Tests are run via `command` steps with `test_type` set — there is no separate `test` step type.
5. **Logical flow**: Steps make logical sense in sequence
6. **Completeness**: The workflow achieves what its description says
7. **Timeouts**: Reasonable timeout values for each step type
8. **Names**: Descriptive, non-empty step names
9. **Command validity**: command steps have real commands (not placeholders)
10. **Check consistency**: check_type and command match (lint -> linter, typecheck -> type checker)
11. **Prompt quality**: Agentic prompts are substantive with specific instructions
12. **Data flow validity**: All `inputs` and `depends_on` references point to existing step IDs. No circular dependencies in `depends_on` chains. `extract` expressions are valid.
13. **CRITICAL — Automated verification required**: verification_steps MUST include at least one deterministic automated step (command or ui_bridge with assert action). A workflow with ONLY prompt-type verification steps is INVALID — this MUST be flagged as a failure.
14. **Code change typecheck**: If the workflow creates or modifies code files (TypeScript, Python, Rust), there MUST be a `command` step with `check_type: "typecheck"` and appropriate command (npx tsc --noEmit, mypy, cargo check). Missing typecheck for code-modifying workflows MUST be flagged.
15. **Web app SDK verification**: If the workflow targets a web app (localhost:3001, localhost:1420) AND verifies interactive UI behavior, there SHOULD be a `ui_bridge` step in verification. For simple health checks or content verification (page loads, contains text), `command` steps with `curl` are sufficient — do NOT flag missing UI Bridge for these.
16. **Agentic-verification coverage**: Every agentic step MUST have at least one verification step that would FAIL if that agentic step's work was NOT done. A tab-existence check does NOT count as coverage for an agentic step that adds content within that tab. Flag as a failure if any agentic step has no corresponding verification.
17. **Out-of-scope enforcement**: If the task description contains "out of scope", "do not change", "do not modify", or similar boundary constraints, there SHOULD be at least one verification step that checks those boundaries were respected. Flag as a warning if missing.
18. **Removal runtime verification**: If any stage removes a page/route/component from a web app, does it include a runtime verification (UI Bridge or curl) that the removed entity is no longer accessible? File-existence checks (test ! -f) alone are insufficient for web apps. Flag as a failure if missing.
19. **Per-stage UI coverage**: In multi-stage workflows, does each stage that modifies UI have its own UI Bridge verification? A UI Bridge check in one stage does NOT cover changes in another stage. Flag as a warning if missing.
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
11. Only use the 3 core step types: command, ui_bridge, prompt. Tests are run via command steps with test_type set — there is no separate test step type.

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
fn build_investigation_setup_prompt(description: &str, referenced_file_contents: &str) -> String {
    let file_section = if referenced_file_contents.is_empty() {
        String::new()
    } else {
        format!(
            r#"

## Referenced File Contents

**CRITICAL: The user's description references the file(s) below. These file contents ARE the task specification. Base your enriched description primarily on their contents — they define what the generated workflow should implement.**

{referenced_file_contents}
"#,
            referenced_file_contents = referenced_file_contents,
        )
    };

    format!(
        r#"You are a codebase investigation AI preparing context for workflow generation.

IMPORTANT: You are the first agent in a workflow generation pipeline. The task description below
describes what a GENERATED WORKFLOW will do when executed by the runner. You are NOT executing
this task — do NOT attempt to run commands, use APIs, interact with services, or take any actions.
Your output feeds into a Builder agent that creates a UnifiedWorkflow JSON specification.

## User's Original Task Description

{description}
{file_section}
## Your Task

Analyze the user's intent in context of the project structure (from discovery context) and produce an **enriched task description**:

1. **Read referenced files first** — if referenced file contents are provided above, those files ARE the task specification. Base your enriched description primarily on their contents, not on keyword matches against the codebase
2. **Preserve the original intent** — do not change what the user wants to accomplish
3. **Identify relevant components** — name specific files, directories, modules, and patterns relevant to the task
4. **Note technical context** — mention frameworks, build tools, test runners, and conventions
5. **Flag potential issues** — dead code paths, missing implementations, broken data flows
6. **Specify concrete targets** — replace vague references with specific file paths and component names
7. **Mention verification approaches** — suggest what should be checked to verify the work
8. **Runtime verification for removals** — for tasks that involve removing pages/routes/components, note that the running application must be checked (not just source files) since build caches, SSR, and routing configurations may still serve removed content

CRITICAL: YOU (the investigation agent) must not run shell commands, make HTTP requests, use APIs,
or interact with any system. You are a text-in, text-out analyst. However, the WORKFLOW that will be
generated from your output WILL have full access to the project filesystem, shell commands, and APIs.
Do NOT ask questions or request permission.
Output ONLY the enriched task description as plain text. No JSON, no code blocks, no prefixes, no questions."#,
        description = description,
        file_section = file_section,
    )
}

/// Public wrapper for `read_referenced_files_from_description` (used by generator.rs).
pub fn read_referenced_files_from_description_pub(description: &str) -> String {
    read_referenced_files_from_description(description)
}

/// Read referenced files from the user's description and return their contents.
///
/// Uses the same `extract_relative_paths` logic as the discovery tools to find
/// file paths in the description, then reads them from disk. Walks up parent
/// directories from the project root to find files in sibling directories.
fn read_referenced_files_from_description(description: &str) -> String {
    let paths = super::discovery_tools::extract_relative_paths_pub(description);
    if paths.is_empty() {
        return String::new();
    }

    let project_path = crate::mcp::shared::current_project_path().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    let mut sections = Vec::new();

    for file_path in &paths {
        let base = std::path::Path::new(&project_path);
        let mut full_path = base.join(file_path);

        // Walk up parent directories if not found at project root
        if !full_path.exists() {
            let mut ancestor = base.parent();
            while let Some(parent) = ancestor {
                let candidate = parent.join(file_path);
                if candidate.exists() {
                    full_path = candidate;
                    break;
                }
                ancestor = parent.parent();
            }
        }

        if full_path.exists() && full_path.is_file() {
            match std::fs::read_to_string(&full_path) {
                Ok(content) => {
                    let max_chars = 15_000;
                    let display_content = if content.len() > max_chars {
                        format!(
                            "{}...\n\n(truncated at {} chars, total {} chars)",
                            &content[..max_chars],
                            max_chars,
                            content.len()
                        )
                    } else {
                        content
                    };
                    sections.push(format!(
                        "### File: {}\n\n```\n{}\n```",
                        file_path, display_content
                    ));
                    tracing::info!(
                        "Meta-workflow: read referenced file {} ({} bytes)",
                        full_path.display(),
                        display_content.len()
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Meta-workflow: failed to read {}: {}",
                        full_path.display(),
                        e
                    );
                }
            }
        } else {
            tracing::debug!(
                "Meta-workflow: referenced file not found: {}",
                full_path.display()
            );
        }
    }

    sections.join("\n\n")
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
2. Preserve all original step IDs AND step names (splitting a step keeps the original ID and name on the first part; new split steps get descriptive names explaining what they verify)
3. Step count may increase (from splitting) but must never decrease
4. If a prompt step is genuinely subjective, keep it as `prompt`
5. For `command` conversions, include `command` and `working_directory`; optionally `check_type` for lint/typecheck/test checks
6. Only 4 valid step types: `command`, `test`, `ui_bridge`, `prompt`

If there are no steps that need hardening, return the workflow unchanged.

Output ONLY the complete, valid UnifiedWorkflow JSON. No markdown, no code fences, no explanations."#.to_string()
}

/// Build the "past fixes" section from resolved findings on meta task runs.
fn build_past_fixes_section(conn: &Connection) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::Connection;

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
