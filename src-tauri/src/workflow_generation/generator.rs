//! Workflow Generator
//!
//! Generates UnifiedWorkflows from natural language descriptions using a
//! 3-agent agentic pipeline:
//!
//! 1. **Builder Agent** — generates the initial workflow JSON from the user's
//!    natural-language description + schema context.
//! 2. **Verification Agent** — reviews every deterministic step for semantic
//!    correctness *without running them*: command syntax, URL validity,
//!    check_type / command consistency, prompt quality, cross-step references,
//!    logical phase flow, etc.
//! 3. **Fixer Agent** — takes the verification report and the current workflow
//!    JSON, then produces a corrected version.
//!
//! Steps 2–3 loop until the verification agent reports zero issues or
//! `max_fix_iterations` is reached.

use crate::ai_provider::{run_prompt_with_routing, AiResponse};
use crate::ai_router::TaskContext;
use crate::context;
use crate::doctor::DoctorHandle;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::hardener::{self, HardeningSummary};
use crate::workflow_generation::schema_context::{build_schema_context, build_schema_context_full};
use rusqlite::Connection;
use crate::workflow_generation::validation::{fix_workflow, validate_workflow, ValidationError};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Request to generate a workflow from natural language
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowRequest {
    /// Natural language description of what the workflow should do
    pub description: String,
    /// Optional category for the generated workflow
    pub category: Option<String>,
    /// Optional tags for the generated workflow
    pub tags: Option<Vec<String>>,

    // === Workflow Configuration Options ===
    /// Maximum iterations for agentic phase (default: 10)
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// AI provider override (claude_cli, anthropic_api, openai_api, gemini_api)
    #[serde(default)]
    pub provider: Option<String>,
    /// Model override (depends on provider)
    #[serde(default)]
    pub model: Option<String>,
    /// Skip AI summary generation at the end (default: false)
    #[serde(default)]
    pub skip_ai_summary: Option<bool>,
    /// Log source selection mode: "default", "ai", "all", or a profile_id
    #[serde(default)]
    pub log_source_selection: Option<String>,
    /// Custom developer prompt template for the workflow
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Whether to auto-include contexts based on task mentions (default: true)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,

    /// Context IDs to resolve and inject into the generation prompt
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    /// Inline context text to inject directly (e.g., pasted CLAUDE.md content)
    #[serde(default)]
    pub inline_context: Option<String>,

    /// Maximum verification→fix iterations (default: 3, 0 = skip verification)
    #[serde(default)]
    pub max_fix_iterations: Option<u32>,
}

/// One pass of the verification→fix loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationIteration {
    /// 1-based iteration number
    pub iteration: u32,
    /// Issues found by the verification agent
    pub issues: Vec<String>,
    /// Whether the fixer was invoked
    pub fix_applied: bool,
    /// Error message if the fixer agent failed (e.g., produced invalid JSON)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_error: Option<String>,
}

/// Response from workflow generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateWorkflowResponse {
    /// The generated workflow (if successful)
    pub workflow: Option<UnifiedWorkflow>,
    /// Structural validation errors (from deterministic validator)
    pub validation_errors: Vec<String>,
    /// Whether the generation was successful
    pub success: bool,
    /// Error message if generation failed
    pub error: Option<String>,
    /// The model that was used for generation (not available for CLI)
    pub model_used: Option<String>,
    /// Details of each verification→fix iteration (empty when skipped)
    #[serde(default)]
    pub verification_iterations: Vec<VerificationIteration>,
    /// Summary of verification hardening (prompt → deterministic conversions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardening_summary: Option<HardeningSummary>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// Generate a workflow from a natural language description using the
/// builder → verification → fixer agentic pipeline.
///
/// When `conn` and optionally `query_embedding` are provided, uses filtered
/// schema context with RAG examples for improved generation quality.
pub fn generate_workflow(
    request: GenerateWorkflowRequest,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
    query_embedding: Option<&[f32]>,
) -> GenerateWorkflowResponse {
    info!(
        "Generating workflow from description: {}",
        &request.description[..request.description.len().min(100)]
    );

    let max_fix_iters = request.max_fix_iterations.unwrap_or(3);

    // ── Step 1: Builder Agent ──────────────────────────────────────────────
    let mut workflow = match run_builder_agent(&request, doctor_handle, conn, query_embedding) {
        Ok(w) => w,
        Err(resp) => return *resp,
    };

    // Apply request overrides
    apply_request_options(&mut workflow, &request);

    // Deterministic auto-fix (UUIDs, timestamps, phase mismatches)
    fix_workflow(&mut workflow);

    // ── Step 2–3: Verification ↔ Fixer loop ────────────────────────────────
    let mut iterations: Vec<VerificationIteration> = Vec::new();

    if max_fix_iters > 0 {
        for iter_num in 1..=max_fix_iters {
            info!("Verification iteration {}/{}", iter_num, max_fix_iters);

            // Run verification agent
            let issues = run_verification_agent(&workflow, &request.description, doctor_handle);

            let issue_count = issues.len();
            info!("Verification found {} issues", issue_count);

            if issue_count == 0 {
                iterations.push(VerificationIteration {
                    iteration: iter_num,
                    issues: vec![],
                    fix_applied: false,
                    fix_error: None,
                });
                info!("Workflow passed verification on iteration {}", iter_num);
                break;
            }

            // Log issues
            for issue in &issues {
                warn!("  verification: {}", issue);
            }

            // Last iteration — record issues but don't fix (no point, nothing will verify again)
            if iter_num == max_fix_iters {
                iterations.push(VerificationIteration {
                    iteration: iter_num,
                    issues,
                    fix_applied: false,
                    fix_error: None,
                });
                warn!(
                    "Max fix iterations reached with {} remaining issues",
                    issue_count
                );
                break;
            }

            // Run fixer agent
            match run_fixer_agent(&workflow, &issues, &request.description, doctor_handle) {
                Ok(fixed) => {
                    iterations.push(VerificationIteration {
                        iteration: iter_num,
                        issues,
                        fix_applied: true,
                        fix_error: None,
                    });
                    workflow = fixed;
                    // Re-apply deterministic fixes on the corrected version
                    fix_workflow(&mut workflow);
                }
                Err(e) => {
                    warn!("Fixer agent failed: {}", e);
                    iterations.push(VerificationIteration {
                        iteration: iter_num,
                        issues,
                        fix_applied: false,
                        fix_error: Some(e),
                    });
                    break;
                }
            }
        }
    }

    // ── Hardener Agent ───────────────────────────────────────────────────
    let (workflow, hardening_summary) =
        hardener::run_hardener_agent(&workflow, &request.description, doctor_handle);

    // ── Final structural validation ────────────────────────────────────────
    let validation_errors: Vec<ValidationError> = validate_workflow(&workflow);
    let validation_error_strings: Vec<String> =
        validation_errors.iter().map(|e| e.to_string()).collect();

    if !validation_errors.is_empty() {
        warn!(
            "Generated workflow has {} structural validation errors",
            validation_errors.len()
        );
    }

    info!(
        "Successfully generated workflow: {} ({} setup, {} verification, {} agentic, {} completion steps, {} verification iterations)",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len(),
        iterations.len(),
    );

    GenerateWorkflowResponse {
        workflow: Some(workflow),
        validation_errors: validation_error_strings,
        success: true,
        error: None,
        model_used: None,
        verification_iterations: iterations,
        hardening_summary,
    }
}

// ============================================================================
// Builder Agent
// ============================================================================

/// Run the builder agent to generate the initial workflow JSON.
fn run_builder_agent(
    request: &GenerateWorkflowRequest,
    doctor_handle: Option<&DoctorHandle>,
    conn: Option<&Connection>,
    query_embedding: Option<&[f32]>,
) -> Result<UnifiedWorkflow, Box<GenerateWorkflowResponse>> {
    let schema_context = if conn.is_some() || query_embedding.is_some() {
        build_schema_context_full(&request.description, conn, query_embedding)
    } else {
        build_schema_context()
    };

    // Resolve saved + inline context
    let mut context_section = String::new();
    if let Some(ref ids) = request.context_ids {
        if !ids.is_empty() {
            let resolved = context::resolve_contexts(ids, false, "", &[], &[]);
            if let Some(formatted) = context::format_contexts_for_prompt(&resolved) {
                context_section.push_str(&formatted);
            }
        }
    }
    if let Some(ref inline) = request.inline_context {
        if !inline.is_empty() {
            context_section.push_str(&format!(
                "<context name=\"User-Provided Context\">\n{}\n</context>\n\n",
                inline
            ));
        }
    }

    let user_prompt = format!(
        r#"## User's Request
{description}

{category_hint}

Generate a complete UnifiedWorkflow JSON that accomplishes this task.

### Quality checklist — ensure your output meets ALL of these:
- Every `shell_command` has a real, syntactically-valid command (no placeholders).
- Every `check` step has a `command` that matches its `check_type` (e.g. lint → eslint/ruff, typecheck → tsc/mypy, format → prettier/black).
- Every `api_request` has a well-formed URL starting with http:// or https://.
- Every `test` step has a valid `test_type` and either a `command` or `code` field.
- Every `prompt` in the agentic phase has substantive, multi-sentence instructions that reference the verification results and explain exactly what to fix.
- If verification steps exist there MUST be at least one agentic `prompt` step.
- `gate` steps only reference `required_steps` IDs that exist in the same phase.
- Step names are descriptive (not "Step 1", "Test", etc.).
- `working_directory` paths look like real absolute or project-relative paths (no placeholders like "/path/to/project").
- If the workflow targets a web application (localhost:3001, localhost:1420, or similar), include a setup step to connect via UI Bridge SDK (POST /ui-bridge/sdk/connect). Use SDK endpoints for element inspection and state checking instead of Playwright when possible.
- Prompt steps that need to inspect or interact with web UI should reference SDK tools (sdk_elements, sdk_snapshot, sdk_ai_execute, sdk_ai_search) rather than Playwright for registered-element interactions.
- To verify page text content (metrics, statuses, headings), use SDK content discovery (sdk_elements with contentOnly/contentTypes filters, or sdk_snapshot) instead of screenshots. Use sdk_page_refresh/sdk_page_navigate for page navigation.

Remember: Return ONLY valid JSON, no markdown code blocks or explanations."#,
        description = request.description,
        category_hint = request
            .category
            .as_ref()
            .map(|c| format!("Use category: {}", c))
            .unwrap_or_default()
    );

    let full_prompt = if context_section.is_empty() {
        format!("{}\n\n{}", schema_context, user_prompt)
    } else {
        format!(
            "{}\n\n{}\n\n{}",
            schema_context, context_section, user_prompt
        )
    };

    let task_context = TaskContext::from_prompt(&full_prompt);
    let ai_result: AiResponse = run_prompt_with_routing(&full_prompt, &task_context, doctor_handle);

    if !ai_result.success {
        error!(
            "Builder agent error: {}",
            ai_result.error.as_deref().unwrap_or("Unknown error")
        );
        return Err(Box::new(GenerateWorkflowResponse {
            workflow: None,
            validation_errors: vec![],
            success: false,
            error: Some(format!(
                "AI provider error: {}",
                ai_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string())
            )),
            model_used: None,
            verification_iterations: vec![],
            hardening_summary: None,
        }));
    }

    debug!("Builder agent response received, parsing JSON...");
    let json_text = extract_json_from_response(&ai_result.output);

    serde_json::from_str::<UnifiedWorkflow>(&json_text).map_err(|e| {
        error!("Failed to parse builder agent JSON: {}", e);
        warn!("Response text: {}", &json_text[..json_text.len().min(500)]);
        Box::new(GenerateWorkflowResponse {
            workflow: None,
            validation_errors: vec![],
            success: false,
            error: Some(format!(
                "Failed to parse generated workflow: {}. The AI may have returned invalid JSON.",
                e
            )),
            model_used: None,
            verification_iterations: vec![],
            hardening_summary: None,
        })
    })
}

// ============================================================================
// Verification Agent
// ============================================================================

/// Run the verification agent to review a workflow for semantic issues.
///
/// Returns a list of human-readable issue descriptions. An empty list means
/// the workflow passed all checks.
fn run_verification_agent(
    workflow: &UnifiedWorkflow,
    user_description: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> Vec<String> {
    let workflow_json = match serde_json::to_string_pretty(workflow) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to serialize workflow for verification: {}", e);
            return vec![format!(
                "Internal error: could not serialize workflow: {}",
                e
            )];
        }
    };

    let prompt = build_verification_prompt(&workflow_json, user_description);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = run_prompt_with_routing(&prompt, &task_context, doctor_handle);

    if !ai_result.success {
        warn!(
            "Verification agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown")
        );
        // Treat AI failure as "no issues found" — don't block the pipeline
        return vec![];
    }

    parse_verification_response(&ai_result.output)
}

/// Build the prompt for the verification agent.
fn build_verification_prompt(workflow_json: &str, user_description: &str) -> String {
    format!(
        r#"You are a workflow verification agent for Qontinui Runner.
Your job is to review a generated UnifiedWorkflow JSON and find semantic errors in the deterministic steps — WITHOUT running anything.

## What to check

For EVERY step in setup_steps, verification_steps, and completion_steps, verify:

### shell_command steps
- `command` is a real, syntactically valid shell command (not a placeholder like "echo TODO" or "/path/to/script")
- `working_directory`, if present, looks like a real path (not "/path/to/project" or "{{placeholder}}")
- `timeout_seconds` is reasonable for the command (not 0, not absurdly high for simple commands)
- `fail_on_error` is appropriate (setup deps should usually fail, cleanups often shouldn't)

### check steps
- `check_type` and `command` are consistent:
  - "lint" → command should run a linter (eslint, ruff, pylint, clippy, etc.)
  - "typecheck" → command should run a type checker (tsc, mypy, pyright, etc.)
  - "format" → command should run a formatter check (prettier, black, rustfmt, etc.)
  - "analyze" → static analysis tool
  - "security" → security scanner
  - "custom_command" → any command is fine
- `command` is non-empty and syntactically valid

### test steps
- Has either `command` (for repository/custom_command) or `code` (for playwright/python)
- `test_type` is one of: playwright, qontinui_vision, python, repository, custom_command
- The command/code looks substantive (not a placeholder)

### api_request steps
- `url` starts with "http://" or "https://" and is a plausible URL (not "http://example.com" unless testing)
- `method` is a valid HTTP method
- If `body` is provided, `content_type` is consistent (JSON body → application/json)
- `assertions` reference valid types ("status_code", "body_contains", etc.)

### prompt steps (especially in agentic_steps)
- Content is substantive — at least 2 sentences with specific instructions
- Agentic prompts reference verification results and describe what to fix
- Not a generic placeholder like "Fix the errors" or "Do the task"

### spec steps
- `spec_group` has a `name` and non-empty `assertions` array
- `element_source` is "control" or "external"
- Each assertion has `assertionType`, `target` with search criteria

### gate steps
- `required_steps` references step IDs that actually exist in the same verification_steps array

### UI Bridge SDK usage
- If the workflow targets a web app (localhost:3001, localhost:1420, or similar React/Next.js app) but does NOT include a setup step to connect via UI Bridge SDK (POST to /ui-bridge/sdk/connect), flag it: "Workflow targets a web app but is missing a UI Bridge SDK connect step in setup — add POST /ui-bridge/sdk/connect to enable direct element access."
- If the workflow targets a web app and uses Playwright (script or test steps) for simple element inspection or clicking when SDK endpoints could be used instead, flag it: "Consider using UI Bridge SDK endpoints instead of Playwright for '{{step name}}' — the SDK provides direct element access without browser overhead."
- If agentic prompt steps mention web UI interaction but don't reference SDK tools (sdk_elements, sdk_snapshot, sdk_ai_execute, sdk_ai_search), flag it: "Agentic prompt '{{step name}}' should reference SDK tools for web UI interaction."

### Cross-step and structural checks
- If there are verification steps, there should be at least one agentic prompt step
- Setup steps should logically prepare for what verification checks
- Step names are descriptive (not "Step 1", "Test", "Check", etc.)
- No duplicate step IDs
- Steps match the user's original request: "{user_description}"

## Output format

If you find issues, return a JSON array of strings, one per issue. Each issue should identify the step by name/index and describe the problem clearly.

If everything looks correct, return an empty array: []

Return ONLY the JSON array. No explanations, no markdown, just the array.

Examples:
["setup_steps[0] 'Install Deps': working_directory '/path/to/project' is a placeholder, not a real path", "agentic_steps[0] 'Fix Issues': prompt content is too vague — needs specific instructions referencing verification results"]
[]

## Workflow JSON to verify

{workflow_json}"#,
        user_description = user_description,
        workflow_json = workflow_json,
    )
}

/// Parse the verification agent's response into a list of issue strings.
fn parse_verification_response(response: &str) -> Vec<String> {
    let json_text = extract_json_array_from_response(response);

    match serde_json::from_str::<Vec<String>>(&json_text) {
        Ok(issues) => issues,
        Err(e) => {
            debug!(
                "Could not parse verification response as JSON array: {} — treating as text",
                e
            );
            // Fall back: if the AI returned free-text instead of JSON, split by lines
            // and filter out empty / boilerplate lines
            let lines: Vec<String> = response
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with("```")
                        && l != "[]"
                        && !l.starts_with("No issues")
                        && !l.to_lowercase().starts_with("everything looks")
                })
                .collect();
            if lines.is_empty() {
                vec![] // treat as "no issues"
            } else {
                lines
            }
        }
    }
}

/// Extract a JSON array from a response that might be wrapped in markdown.
fn extract_json_array_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try markdown code block
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let json_start = after.find('\n').map(|p| p + 1).unwrap_or(0);
        if let Some(end) = after[json_start..].find("```") {
            return after[json_start..json_start + end].trim().to_string();
        }
    }

    // Try to find a JSON array directly
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

// ============================================================================
// Fixer Agent
// ============================================================================

/// Run the fixer agent, returning a corrected workflow or an error message.
fn run_fixer_agent(
    workflow: &UnifiedWorkflow,
    issues: &[String],
    user_description: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> Result<UnifiedWorkflow, String> {
    let workflow_json = serde_json::to_string_pretty(workflow)
        .map_err(|e| format!("Failed to serialize workflow: {}", e))?;

    let prompt = build_fix_prompt(&workflow_json, issues, user_description);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = run_prompt_with_routing(&prompt, &task_context, doctor_handle);

    if !ai_result.success {
        return Err(format!(
            "Fixer AI error: {}",
            ai_result.error.unwrap_or_else(|| "unknown".to_string())
        ));
    }

    let json_text = extract_json_from_response(&ai_result.output);
    serde_json::from_str::<UnifiedWorkflow>(&json_text)
        .map_err(|e| format!("Fixer produced invalid JSON: {}", e))
}

/// Build the prompt for the fixer agent.
fn build_fix_prompt(workflow_json: &str, issues: &[String], user_description: &str) -> String {
    let issues_text = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| format!("{}. {}", i + 1, issue))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a workflow fixer agent for Qontinui Runner.

## Your task

A verification agent found issues in the workflow below. Fix ALL of them and return the corrected, complete UnifiedWorkflow JSON.

## Rules
- Fix every listed issue. Do NOT skip any.
- Preserve the overall structure, step ordering, IDs, and intent of the workflow.
- Do NOT add new steps unless an issue specifically requires it (e.g., "missing agentic step").
- Do NOT remove steps unless an issue specifically says to.
- All UUIDs must be valid v4 format.
- All `phase` fields must match the array they're in.
- Return ONLY valid JSON — no markdown, no explanations.

## The user's original request
{user_description}

## Issues to fix
{issues_text}

## Current workflow JSON
{workflow_json}"#,
        user_description = user_description,
        issues_text = issues_text,
        workflow_json = workflow_json,
    )
}

// ============================================================================
// Helpers
// ============================================================================

/// Apply the request's override options onto the parsed workflow.
fn apply_request_options(workflow: &mut UnifiedWorkflow, request: &GenerateWorkflowRequest) {
    if let Some(ref category) = request.category {
        workflow.category = category.clone();
    }
    if let Some(ref tags) = request.tags {
        workflow.tags = tags.clone();
    }
    if let Some(max_iterations) = request.max_iterations {
        workflow.max_iterations = max_iterations;
    }
    if let Some(ref provider) = request.provider {
        workflow.provider = Some(provider.clone());
    }
    if let Some(ref model) = request.model {
        workflow.model = Some(model.clone());
    }
    if let Some(skip_ai_summary) = request.skip_ai_summary {
        workflow.skip_ai_summary = skip_ai_summary;
    }
    if let Some(ref log_source) = request.log_source_selection {
        use crate::unified_workflows::LogSourceSelection;
        workflow.log_source_selection =
            if log_source == "default" || log_source == "ai" || log_source == "all" {
                LogSourceSelection::Mode(log_source.clone())
            } else {
                LogSourceSelection::Profile {
                    profile_id: log_source.clone(),
                }
            };
    }
    if let Some(ref prompt_template) = request.prompt_template {
        workflow.prompt_template = Some(prompt_template.clone());
    }
    if let Some(auto_include) = request.auto_include_contexts {
        workflow.auto_include_contexts = auto_include;
    }
}

/// Extract JSON from AI response, handling markdown code blocks.
///
/// AI models often wrap JSON output in markdown code fences or add
/// explanatory text before/after. This function extracts the JSON
/// content by trying, in order:
/// 1. JSON in a ```json code block
/// 2. JSON in a generic ``` code block
/// 3. First `{` to last `}` in the text
/// 4. Original text (trimmed) as fallback
pub fn extract_json_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try to find JSON in markdown code block
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }

    // Try to find JSON in generic code block
    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        let json_start = if let Some(newline_pos) = after_backticks.find('\n') {
            newline_pos + 1
        } else {
            0
        };
        if let Some(end) = after_backticks[json_start..].find("```") {
            return after_backticks[json_start..json_start + end]
                .trim()
                .to_string();
        }
    }

    // Try to find JSON object directly
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return trimmed[start..=end].to_string();
            }
        }
    }

    trimmed.to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_from_code_block() {
        let response = r#"Here's the workflow:

```json
{"name": "test"}
```

Hope this helps!"#;

        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }

    #[test]
    fn test_extract_json_direct() {
        let response = r#"{"name": "test", "id": "123"}"#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test", "id": "123"}"#);
    }

    #[test]
    fn test_extract_json_with_text() {
        let response =
            r#"Sure, here is the workflow: {"name": "test"} Let me know if you need changes."#;
        let json = extract_json_from_response(response);
        assert_eq!(json, r#"{"name": "test"}"#);
    }

    #[test]
    fn test_extract_json_array_from_response() {
        let response = r#"```json
["issue 1", "issue 2"]
```"#;
        let json = extract_json_array_from_response(response);
        assert_eq!(json, r#"["issue 1", "issue 2"]"#);
    }

    #[test]
    fn test_extract_json_array_direct() {
        let response = r#"["issue 1"]"#;
        let json = extract_json_array_from_response(response);
        assert_eq!(json, r#"["issue 1"]"#);
    }

    #[test]
    fn test_parse_verification_empty() {
        let issues = parse_verification_response("[]");
        assert!(issues.is_empty());
    }

    #[test]
    fn test_parse_verification_issues() {
        let response = r#"["bad command in step 0", "missing url"]"#;
        let issues = parse_verification_response(response);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0], "bad command in step 0");
    }

    #[test]
    fn test_parse_verification_no_issues_text() {
        // AI might respond with free text instead of JSON
        let issues = parse_verification_response("No issues found. Everything looks good.");
        assert!(issues.is_empty());
    }
}
