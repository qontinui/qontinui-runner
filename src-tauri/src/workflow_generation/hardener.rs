//! Verification Hardener Sub-Agent
//!
//! Post-processes generated workflows to convert prompt-based verification steps
//! to deterministic equivalents (`api_request`, `spec`, `check`) where possible.
//!
//! ## Pipeline placement
//!
//! ```text
//! Builder Agent → fix_workflow() → [Verification → Fixer] × N → Hardener → validate_workflow()
//! ```
//!
//! The hardener runs once after the verification/fixer loop, best-effort.
//! On error it falls back to the original workflow.

use crate::ai_provider::{run_prompt_with_routing, AiResponse};
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::generator::extract_json_from_response;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Summary of hardening conversions applied to a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningSummary {
    /// Number of prompt steps converted to deterministic steps
    pub converted_count: usize,
    /// Number of prompt steps kept with advisory notes
    pub kept_as_prompt_count: usize,
    /// Details of each conversion
    pub conversions: Vec<HardeningConversion>,
}

/// A single hardening conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningConversion {
    /// Step ID that was converted
    pub step_id: String,
    /// Original step name
    pub original_name: String,
    /// Original type (always "prompt")
    pub original_type: String,
    /// New type after conversion
    pub new_type: String,
    /// Brief explanation of the conversion
    pub explanation: String,
}

// ============================================================================
// Context extraction
// ============================================================================

/// Application context extracted from the workflow to guide hardening.
#[derive(Debug)]
struct AppContext {
    /// Whether the workflow targets a web app (localhost:3001 or localhost:1420)
    targets_web_app: bool,
    /// Whether setup includes a UI Bridge SDK connect step
    has_sdk_connect: bool,
    /// Working directories referenced in the workflow
    working_directories: Vec<String>,
    /// Language hints from check_type values
    language_hints: Vec<String>,
}

impl AppContext {
    /// Extract application context from a workflow and its description.
    fn from_workflow(workflow: &UnifiedWorkflow, _description: &str) -> Self {
        let workflow_json = serde_json::to_string(workflow).unwrap_or_default();

        let targets_web_app = workflow_json.contains("localhost:3001")
            || workflow_json.contains("localhost:1420");

        let has_sdk_connect = workflow_json.contains("ui-bridge/sdk/connect");

        let mut working_directories = Vec::new();
        let mut language_hints = Vec::new();

        let all_steps: Vec<&Value> = workflow
            .setup_steps
            .iter()
            .chain(workflow.verification_steps.iter())
            .chain(workflow.agentic_steps.iter())
            .chain(workflow.completion_steps.iter())
            .collect();

        for step in &all_steps {
            if let Some(wd) = step
                .get("working_directory")
                .and_then(|v| v.as_str())
            {
                if !working_directories.contains(&wd.to_string()) {
                    working_directories.push(wd.to_string());
                }
            }
            if let Some(ct) = step.get("check_type").and_then(|v| v.as_str()) {
                let hint = match ct {
                    "typecheck" => {
                        // Try to determine language from command
                        if let Some(cmd) = step.get("command").and_then(|v| v.as_str()) {
                            if cmd.contains("tsc") || cmd.contains("typescript") {
                                "typescript"
                            } else if cmd.contains("mypy") || cmd.contains("pyright") {
                                "python"
                            } else if cmd.contains("cargo") {
                                "rust"
                            } else {
                                "unknown"
                            }
                        } else {
                            "unknown"
                        }
                    }
                    "lint" | "format" | "analyze" | "security" => ct,
                    _ => continue,
                };
                if !language_hints.contains(&hint.to_string()) {
                    language_hints.push(hint.to_string());
                }
            }
        }

        Self {
            targets_web_app,
            has_sdk_connect,
            working_directories,
            language_hints,
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Returns true if any prompt-type steps exist in `verification_steps`.
pub fn should_harden_verification(workflow: &UnifiedWorkflow) -> bool {
    workflow.verification_steps.iter().any(|step| {
        step.get("type")
            .and_then(|v| v.as_str())
            .map(|t| t == "prompt")
            .unwrap_or(false)
    })
}

/// Run the hardener agent to convert prompt verification steps to deterministic equivalents.
///
/// Non-fatal: returns the original workflow on any error.
pub fn run_hardener_agent(
    workflow: &UnifiedWorkflow,
    description: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> (UnifiedWorkflow, Option<HardeningSummary>) {
    if !should_harden_verification(workflow) {
        debug!("No prompt verification steps found, skipping hardener");
        return (workflow.clone(), None);
    }

    let prompt_step_count = workflow
        .verification_steps
        .iter()
        .filter(|s| {
            s.get("type")
                .and_then(|v| v.as_str())
                .map(|t| t == "prompt")
                .unwrap_or(false)
        })
        .count();

    info!(
        "Running hardener agent on {} prompt verification steps",
        prompt_step_count
    );

    let app_context = AppContext::from_workflow(workflow, description);
    let workflow_json = match serde_json::to_string_pretty(workflow) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize workflow for hardening: {}", e);
            return (workflow.clone(), None);
        }
    };

    let prompt = build_hardener_prompt(&workflow_json, description, &app_context);
    let task_context = TaskContext::from_prompt(&prompt);
    let ai_result: AiResponse = run_prompt_with_routing(&prompt, &task_context, doctor_handle);

    if !ai_result.success {
        warn!(
            "Hardener agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown")
        );
        return (workflow.clone(), None);
    }

    // Parse the response
    let json_text = extract_json_from_response(&ai_result.output);
    let hardened: UnifiedWorkflow = match serde_json::from_str(&json_text) {
        Ok(w) => w,
        Err(e) => {
            warn!("Hardener produced invalid JSON: {}", e);
            return (workflow.clone(), None);
        }
    };

    // Safety checks
    if let Some(error) = validate_hardened_output(workflow, &hardened) {
        warn!("Hardener safety check failed: {}", error);
        return (workflow.clone(), None);
    }

    // Build summary by comparing original and hardened verification steps
    let summary = build_summary(workflow, &hardened);

    info!(
        "Hardener converted {} prompt steps, kept {} as prompt",
        summary.converted_count, summary.kept_as_prompt_count
    );

    (hardened, Some(summary))
}

// ============================================================================
// Prompt builder
// ============================================================================

fn build_hardener_prompt(workflow_json: &str, description: &str, app_context: &AppContext) -> String {
    let mut prompt = format!(
        r#"You are a verification hardener agent for Qontinui Runner.

Your job is to convert prompt-type verification steps into deterministic equivalents where possible.
The workflow below was generated for this task: "{description}"

## Conversion Rules

Convert `prompt` verification steps using these mappings:

| Prompt check type | Convert to | Method |
|---|---|---|
| UI element presence/structure | `api_request` | UI Bridge SDK `GET /ui-bridge/sdk/elements` with `body_contains` assertion |
| Content/text on page | `api_request` | UI Bridge SDK `GET /ui-bridge/sdk/ai/search` with `body_contains` assertion |
| File existence | `check` | `custom_command` with `test -f <path>` |
| File content | `check` | `custom_command` with `grep -q <pattern> <file>` |
| Code quality (lint) | `check` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `check` | `check_type: "typecheck"` with appropriate command |
| API health/response | `api_request` | Direct HTTP with `status_code` assertion |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |
"#,
        description = description,
    );

    // Add context-specific guidance
    if app_context.targets_web_app {
        prompt.push_str("\n### Web App Context\n");
        prompt.push_str("This workflow targets a web application. ");
        if app_context.has_sdk_connect {
            prompt.push_str("UI Bridge SDK is connected in setup, so use SDK endpoints for UI verification:\n");
            prompt.push_str("- Element presence: `GET http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true` with `body_contains` assertion\n");
            prompt.push_str("- Text search: `GET http://localhost:9876/ui-bridge/sdk/ai/search?query=<search_term>` with `body_contains` assertion\n");
        } else {
            prompt.push_str("No SDK connect step found — prefer file-based or API health checks.\n");
        }
    }

    if !app_context.working_directories.is_empty() {
        prompt.push_str(&format!(
            "\n### Working Directories\nKnown directories: {}\n",
            app_context.working_directories.join(", ")
        ));
    }

    if !app_context.language_hints.is_empty() {
        prompt.push_str(&format!(
            "\n### Languages Detected\n{}\n",
            app_context.language_hints.join(", ")
        ));
    }

    prompt.push_str(&format!(
        r#"
## Critical Rules

1. **ONLY modify verification_steps** — do NOT change setup_steps, agentic_steps, or completion_steps
2. **Preserve step count** — output must have the same number of verification steps
3. **Preserve step IDs** — every step must keep its original `id` field
4. **Preserve step order** — steps must remain in the same position
5. If a prompt step is genuinely subjective (e.g., "Is the UX intuitive?", "Is the tone appropriate?"), keep it as `prompt` type
6. Every converted step must have all required fields for its new type
7. For `api_request` conversions, include `assertions` array with at least a `status_code` check
8. For `check` conversions, include `check_type`, `command`, and `working_directory`

## Output

Return ONLY the complete, valid UnifiedWorkflow JSON with the hardened verification steps.
No markdown, no code fences, no explanations.

## Current Workflow JSON

{workflow_json}"#,
        workflow_json = workflow_json,
    ));

    prompt
}

// ============================================================================
// Safety validation
// ============================================================================

/// Validate that the hardened output preserves critical invariants.
/// Returns None if valid, Some(error_message) if invalid.
fn validate_hardened_output(
    original: &UnifiedWorkflow,
    hardened: &UnifiedWorkflow,
) -> Option<String> {
    // Step count must match
    if original.verification_steps.len() != hardened.verification_steps.len() {
        return Some(format!(
            "Step count changed: {} -> {}",
            original.verification_steps.len(),
            hardened.verification_steps.len()
        ));
    }

    // Step IDs must be preserved in order
    for (i, (orig, hard)) in original
        .verification_steps
        .iter()
        .zip(hardened.verification_steps.iter())
        .enumerate()
    {
        let orig_id = orig.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let hard_id = hard.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if orig_id != hard_id {
            return Some(format!(
                "Step ID changed at index {}: '{}' -> '{}'",
                i, orig_id, hard_id
            ));
        }
    }

    // Non-verification phases must be unchanged
    if original.setup_steps != hardened.setup_steps {
        return Some("setup_steps were modified".to_string());
    }
    if original.agentic_steps != hardened.agentic_steps {
        return Some("agentic_steps were modified".to_string());
    }
    if original.completion_steps != hardened.completion_steps {
        return Some("completion_steps were modified".to_string());
    }

    None
}

// ============================================================================
// Summary builder
// ============================================================================

fn build_summary(original: &UnifiedWorkflow, hardened: &UnifiedWorkflow) -> HardeningSummary {
    let mut conversions = Vec::new();
    let mut converted_count = 0;
    let mut kept_as_prompt_count = 0;

    for (orig, hard) in original
        .verification_steps
        .iter()
        .zip(hardened.verification_steps.iter())
    {
        let orig_type = orig
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let hard_type = hard
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if orig_type != "prompt" {
            continue; // Not a candidate
        }

        if hard_type != "prompt" {
            converted_count += 1;
            conversions.push(HardeningConversion {
                step_id: orig
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                original_name: orig
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                original_type: "prompt".to_string(),
                new_type: hard_type.to_string(),
                explanation: format!("Converted from prompt to {}", hard_type),
            });
        } else {
            kept_as_prompt_count += 1;
        }
    }

    HardeningSummary {
        converted_count,
        kept_as_prompt_count,
        conversions,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_test_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        UnifiedWorkflow {
            id: "test-id".to_string(),
            name: "Test Workflow".to_string(),
            description: "Test".to_string(),
            category: "testing".to_string(),
            tags: vec![],
            setup_steps: vec![],
            verification_steps,
            agentic_steps: vec![],
            completion_steps: vec![],
            max_iterations: 3,
            timeout_seconds: None,
            provider: None,
            model: None,
            skip_ai_summary: false,
            targeted_error_ids: vec![],
            log_source_selection: Default::default(),
            context_ids: vec![],
            disabled_context_ids: vec![],
            auto_include_contexts: true,
            prompt_template: None,
            log_watch_enabled: false,
            health_check_enabled: false,
            health_check_urls: vec![],
            preflight_check_enabled: false,
            generated_by_task_run_id: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_should_harden_returns_false_for_all_deterministic() {
        let workflow = make_test_workflow(vec![
            json!({
                "id": "step-1",
                "type": "check",
                "check_type": "typecheck",
                "command": "npx tsc --noEmit"
            }),
            json!({
                "id": "step-2",
                "type": "api_request",
                "method": "GET",
                "url": "http://localhost:3001/health"
            }),
        ]);

        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_true_for_prompt_verification() {
        let workflow = make_test_workflow(vec![
            json!({
                "id": "step-1",
                "type": "check",
                "check_type": "typecheck",
                "command": "npx tsc --noEmit"
            }),
            json!({
                "id": "step-2",
                "type": "prompt",
                "phase": "verification",
                "content": "Check if the button exists on the page"
            }),
        ]);

        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_false_for_empty_verification() {
        let workflow = make_test_workflow(vec![]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_validate_hardened_rejects_step_count_change() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "prompt"}),
            json!({"id": "b", "type": "prompt"}),
        ]);
        let hardened = make_test_workflow(vec![json!({"id": "a", "type": "check"})]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("Step count changed"));
    }

    #[test]
    fn test_validate_hardened_rejects_id_change() {
        let original = make_test_workflow(vec![
            json!({"id": "step-1", "type": "prompt"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-2", "type": "check"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("Step ID changed"));
    }

    #[test]
    fn test_validate_hardened_rejects_setup_modification() {
        let mut original = make_test_workflow(vec![json!({"id": "a", "type": "prompt"})]);
        original.setup_steps = vec![json!({"id": "setup-1", "type": "shell_command"})];

        let mut hardened = make_test_workflow(vec![json!({"id": "a", "type": "check"})]);
        hardened.setup_steps = vec![json!({"id": "setup-1", "type": "api_request"})]; // changed

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("setup_steps were modified"));
    }

    #[test]
    fn test_validate_hardened_accepts_valid_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "step-1", "type": "prompt", "content": "Check button"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-1", "type": "api_request", "method": "GET", "url": "http://localhost:9876/ui-bridge/sdk/elements"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    #[test]
    fn test_build_summary_tracks_conversions() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "prompt"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "check"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "check"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}), // kept
            json!({"id": "s3", "name": "Lint check", "type": "check"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert_eq!(summary.kept_as_prompt_count, 1);
        assert_eq!(summary.conversions.len(), 1);
        assert_eq!(summary.conversions[0].step_id, "s1");
        assert_eq!(summary.conversions[0].new_type, "check");
    }

    #[test]
    fn test_app_context_detects_web_app() {
        let mut workflow = make_test_workflow(vec![]);
        workflow.setup_steps = vec![json!({
            "id": "s1",
            "type": "api_request",
            "url": "http://localhost:3001/api/test"
        })];

        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.targets_web_app);
    }

    #[test]
    fn test_app_context_detects_sdk_connect() {
        let mut workflow = make_test_workflow(vec![]);
        workflow.setup_steps = vec![json!({
            "id": "s1",
            "type": "api_request",
            "url": "http://localhost:9876/ui-bridge/sdk/connect",
            "method": "POST"
        })];

        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.has_sdk_connect);
    }
}
