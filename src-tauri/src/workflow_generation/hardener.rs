//! Verification Hardener Sub-Agent
//!
//! Post-processes generated workflows to strengthen verification steps:
//!
//! 1. Converts `prompt` verification steps to deterministic equivalents
//!    (`command` with check_type/curl, `ui_bridge` with assert) where possible.
//! 2. Converts Playwright-based UI checks to `command` steps using curl to
//!    UI Bridge SDK endpoints when the SDK is connected — SDK checks are
//!    faster, more reliable, and don't require Playwright browser installation.
//! 3. Strengthens weak SDK verification commands (e.g., curl without grep)
//!    to include content checks for meaningful verification.
//!
//! ## Pipeline placement
//!
//! ```text
//! Builder Agent → fix_workflow() → [Verification → Fixer] × N → Hardener → validate_workflow()
//! ```
//!
//! The hardener runs once after the verification/fixer loop, best-effort.
//! On error it falls back to the original workflow.

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::skills::SkillRegistry;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::generator::extract_json_from_response;
use crate::workflow_generation::rules;
use crate::workflow_generation::schema_context::{
    format_skills_for_generator, format_skills_for_generator_filtered,
};
use crate::database::pg::PgDb;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

// ============================================================================
// Public types
// ============================================================================

/// Summary of hardening conversions applied to a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardeningSummary {
    /// Number of steps converted to better alternatives
    pub converted_count: usize,
    /// Number of prompt steps kept (genuinely subjective)
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
    /// Original step type
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
    /// Target URL from setup page navigation (if any)
    setup_navigate_url: Option<String>,
    /// Working directories referenced in the workflow
    working_directories: Vec<String>,
    /// Language hints from check_type values
    language_hints: Vec<String>,
}

impl AppContext {
    /// Extract application context from a workflow and its description.
    fn from_workflow(workflow: &UnifiedWorkflow, _description: &str) -> Self {
        let workflow_json = serde_json::to_string(workflow).unwrap_or_default();

        let targets_web_app =
            workflow_json.contains("localhost:3001") || workflow_json.contains("localhost:1420");

        let has_sdk_connect = workflow_json.contains("ui-bridge/sdk/connect");

        // Extract navigation URL from setup steps
        let setup_navigate_url = workflow.setup_steps.iter().find_map(|step| {
            // Check command field for curl to navigate endpoint
            let cmd = step
                .get("command")
                .or_else(|| step.get("shell_command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cmd.contains("ui-bridge/sdk/page/navigate") {
                // Try to extract the URL from the -d JSON body in the curl command
                if let Some(d_pos) = cmd.find("-d") {
                    let after_d = &cmd[d_pos + 2..].trim_start();
                    // Find JSON body (single-quoted or double-quoted)
                    let body_str = if let Some(stripped) = after_d.strip_prefix('\'') {
                        stripped.split('\'').next()
                    } else if let Some(stripped) = after_d.strip_prefix('"') {
                        stripped.split('"').next()
                    } else {
                        after_d.split_whitespace().next()
                    };
                    body_str
                        .and_then(|body| serde_json::from_str::<Value>(body).ok())
                        .and_then(|parsed| {
                            parsed.get("url").and_then(|u| u.as_str()).map(String::from)
                        })
                } else {
                    None
                }
            } else {
                None
            }
        });

        let mut working_directories = Vec::new();
        let mut language_hints = Vec::new();

        let all_steps: Vec<&Value> = workflow
            .setup_steps
            .iter()
            .chain(workflow.verification_steps.iter())
            .chain(workflow.agentic_steps.iter())
            .chain(workflow.completion_steps.iter())
            .chain(workflow.stages.iter().flat_map(|stage| {
                stage
                    .setup_steps
                    .iter()
                    .chain(stage.verification_steps.iter())
                    .chain(stage.agentic_steps.iter())
                    .chain(stage.completion_steps.iter())
            }))
            .collect();

        for step in &all_steps {
            if let Some(wd) = step.get("working_directory").and_then(|v| v.as_str()) {
                if !working_directories.contains(&wd.to_string()) {
                    working_directories.push(wd.to_string());
                }
            }
            if let Some(ct) = step.get("check_type").and_then(|v| v.as_str()) {
                let hint = match ct {
                    "typecheck" => {
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
            setup_navigate_url,
            working_directories,
            language_hints,
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Returns true if any verification steps could benefit from hardening.
///
/// Checks for:
/// - `prompt` steps (should be deterministic where possible)
/// - `command` steps with mode "test" and `playwright` test_type when SDK is connected
/// - `command` steps with mode "shell" hitting SDK endpoints without content validation
/// - Agentic steps that lack corresponding verification coverage
pub fn should_harden_verification(workflow: &UnifiedWorkflow) -> bool {
    let has_sdk_connect = {
        let json = serde_json::to_string(workflow).unwrap_or_default();
        json.contains("ui-bridge/sdk/connect")
    };

    let has_hardenable_steps = workflow.verification_steps.iter().any(|step| {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mode = step.get("mode").and_then(|v| v.as_str()).unwrap_or("");

        match step_type {
            // Prompt steps are always candidates
            "prompt" => true,

            // Command steps with mode "test" are candidates when SDK is connected
            "command" if mode == "test" || step.get("test_type").is_some() => {
                if !has_sdk_connect {
                    return false;
                }
                let test_type = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                // Playwright UI tests can be replaced by SDK checks
                test_type == "playwright" || is_ui_verification_test(step)
            }

            // Command steps with mode "shell" that call SDK with weak assertions
            "command" if mode == "shell" && has_sdk_connect => {
                let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if !cmd.contains("ui-bridge/sdk") {
                    return false;
                }
                // If using curl to SDK but not grepping for specific content, it's weak
                !cmd.contains("grep") && !cmd.contains("jq")
            }

            _ => false,
        }
    });

    if has_hardenable_steps {
        return true;
    }

    // Check stage verification steps for hardenable candidates
    let has_hardenable_stage_steps = workflow.stages.iter().any(|stage| {
        stage.verification_steps.iter().any(|step| {
            let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let mode = step.get("mode").and_then(|v| v.as_str()).unwrap_or("");

            match step_type {
                "prompt" => true,
                "command" if mode == "test" || step.get("test_type").is_some() => {
                    if !has_sdk_connect {
                        return false;
                    }
                    let test_type = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                    test_type == "playwright" || is_ui_verification_test(step)
                }
                "command" if mode == "shell" && has_sdk_connect => {
                    let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    if !cmd.contains("ui-bridge/sdk") {
                        return false;
                    }
                    !cmd.contains("grep") && !cmd.contains("jq")
                }
                _ => false,
            }
        })
    });

    if has_hardenable_stage_steps {
        return true;
    }

    // Check for agentic-verification coverage gaps:
    // If there are more agentic steps than deterministic verification steps,
    // there's likely a gap where some agentic work has no verification coverage
    if !workflow.agentic_steps.is_empty() {
        let deterministic_verification_count = workflow
            .verification_steps
            .iter()
            .filter(|s| {
                let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
                t != "prompt"
            })
            .count();

        // Heuristic: each agentic step should have at least one dedicated check
        if workflow.agentic_steps.len() > deterministic_verification_count {
            return true;
        }
    }

    // Check for agentic-verification coverage gaps within stages
    for stage in &workflow.stages {
        if !stage.agentic_steps.is_empty() {
            let deterministic_count = stage
                .verification_steps
                .iter()
                .filter(|s| {
                    let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    t != "prompt"
                })
                .count();

            if stage.agentic_steps.len() > deterministic_count {
                return true;
            }
        }
    }

    false
}

/// Check if a test step is doing UI verification (vs. unit/integration tests).
fn is_ui_verification_test(step: &Value) -> bool {
    let command = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let description = step
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let combined = format!("{} {} {}", command, description, name).to_lowercase();

    // UI-related keywords suggest this test verifies UI state
    combined.contains("ui")
        || combined.contains("element")
        || combined.contains("render")
        || combined.contains("visual")
        || combined.contains("page")
        || combined.contains("tab")
        || combined.contains("button")
        || combined.contains("component")
        || combined.contains("graph editor")
        || combined.contains("thumbnail")
}

/// Run the hardener agent to strengthen verification steps.
///
/// Converts prompt steps to deterministic equivalents, replaces Playwright UI
/// tests with SDK checks when the SDK is available, and upgrades weak assertions.
///
/// Non-fatal: returns the original workflow on any error.
/// Returns `(workflow, summary, hardener_prompt)`.
pub fn run_hardener_agent(
    workflow: &UnifiedWorkflow,
    description: &str,
    doctor_handle: Option<&DoctorHandle>,
    pg_db: Option<&Arc<PgDb>>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    skill_registry: &SkillRegistry,
    insights_section: Option<&str>,
    tool_tags: Option<&[String]>,
    constitution: Option<&str>,
) -> (UnifiedWorkflow, Option<HardeningSummary>, Option<String>) {
    // Step 0: Apply deterministic fixups before AI hardening
    let mut workflow = fix_sdk_urls(workflow);

    // Step 0b: Sanitize Python commands (fix f-string escaping issues)
    let mut sanitize_total = 0;
    sanitize_total += sanitize_commands_in_steps(&mut workflow.setup_steps);
    sanitize_total += sanitize_commands_in_steps(&mut workflow.verification_steps);
    sanitize_total += sanitize_commands_in_steps(&mut workflow.completion_steps);
    // Sanitize stage steps
    for stage in workflow.stages.iter_mut() {
        sanitize_total += sanitize_commands_in_steps(&mut stage.setup_steps);
        sanitize_total += sanitize_commands_in_steps(&mut stage.verification_steps);
        sanitize_total += sanitize_commands_in_steps(&mut stage.completion_steps);
    }
    if sanitize_total > 0 {
        info!(
            "Command sanitizer: fixed {} steps with Python escaping issues",
            sanitize_total
        );
    }

    if !should_harden_verification(&workflow) {
        debug!("No hardenable verification steps found, skipping");
        return (workflow, None, None);
    }

    let candidate_count = count_candidates(&workflow);
    info!(
        "Running hardener agent on {} candidate verification steps",
        candidate_count
    );

    let app_context = AppContext::from_workflow(&workflow, description);
    let workflow_json = match serde_json::to_string_pretty(&workflow) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize workflow for hardening: {}", e);
            return (workflow, None, None);
        }
    };

    let prompt = build_hardener_prompt(
        &workflow_json,
        description,
        &app_context,
        pg_db,
        skill_registry,
        insights_section,
        tool_tags,
        constitution,
    );
    let task_context = TaskContext::from_prompt(&prompt);

    // Use middleware chain for deterministic post-processing of AI output.
    // This replaces the ad-hoc sanitize/fix calls that were previously applied
    // after the AI call (the middleware handles them in a composable chain).
    let middleware = build_hardener_middleware();
    let mw_ctx = MiddlewareContext::new("hardener");
    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_middleware(
        &prompt,
        &task_context,
        doctor_handle,
        model_override,
        provider_override,
        None,
        None,
        None,
        None,
        &middleware,
        &mw_ctx,
    );

    if !ai_result.success {
        warn!(
            "Hardener agent failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown")
        );
        return (workflow, None, Some(prompt));
    }

    // Parse the response
    let json_text = extract_json_from_response(&ai_result.output);
    let mut hardened: UnifiedWorkflow = match serde_json::from_str(&json_text) {
        Ok(w) => w,
        Err(e) => {
            warn!("Hardener produced invalid JSON: {}", e);
            return (workflow, None, Some(prompt));
        }
    };

    // Safety checks
    if let Some(error) = validate_hardened_output(&workflow, &hardened) {
        warn!("Hardener safety check failed: {}", error);
        return (workflow, None, Some(prompt));
    }

    // NOTE: Post-hardener command sanitization and SDK URL fixup are now handled
    // by the middleware chain (CommandSanitizer + SdkUrlSanitizer) applied around
    // the AI call above. The middleware operates on the raw AI response before
    // parsing, so the workflow we get here is already sanitized.

    // Re-inject any regression steps that the AI may have dropped.
    // This is a safety net: the prompt tells the AI to preserve them, and
    // validate_hardened_output checks for them, but if a regression step was
    // removed AND the validation was somehow bypassed (e.g., future changes),
    // this ensures they are always present in the final output.
    let original_regression_steps: Vec<&Value> = workflow
        .verification_steps
        .iter()
        .filter(|s| s.get("regression_issue_id").is_some())
        .collect();

    if !original_regression_steps.is_empty() {
        let hardened_regression_ids: std::collections::HashSet<String> = hardened
            .verification_steps
            .iter()
            .filter_map(|s| {
                s.get("regression_issue_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();

        for step in original_regression_steps {
            if let Some(issue_id) = step.get("regression_issue_id").and_then(|v| v.as_str()) {
                if !hardened_regression_ids.contains(issue_id) {
                    hardened.verification_steps.push(step.clone());
                    info!(
                        "Re-injected regression step for issue '{}' after hardening",
                        issue_id
                    );
                }
            }
        }
    }

    // Re-inject regression steps in stages
    for (idx, orig_stage) in workflow.stages.iter().enumerate() {
        let stage_regression_steps: Vec<&Value> = orig_stage
            .verification_steps
            .iter()
            .filter(|s| s.get("regression_issue_id").is_some())
            .collect();

        if !stage_regression_steps.is_empty() {
            if let Some(hard_stage) = hardened.stages.get_mut(idx) {
                let hard_stage_regression_ids: std::collections::HashSet<String> = hard_stage
                    .verification_steps
                    .iter()
                    .filter_map(|s| {
                        s.get("regression_issue_id")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .collect();

                for step in stage_regression_steps {
                    if let Some(issue_id) = step.get("regression_issue_id").and_then(|v| v.as_str())
                    {
                        if !hard_stage_regression_ids.contains(issue_id) {
                            hard_stage.verification_steps.push(step.clone());
                            info!(
                                "Re-injected regression step for issue '{}' in stages[{}] after hardening",
                                issue_id, idx
                            );
                        }
                    }
                }
            }
        }
    }

    // Build summary by comparing original and hardened verification steps
    let summary = build_summary(&workflow, &hardened);

    info!(
        "Hardener converted {} steps, kept {} as prompt",
        summary.converted_count, summary.kept_as_prompt_count
    );

    (hardened, Some(summary), Some(prompt))
}

// ============================================================================
// Middleware implementations (wrapping existing sanitizer functions)
// ============================================================================

use crate::ai_provider::middleware::{AiMiddleware, AiMiddlewareChain, MiddlewareContext};

/// Post-call middleware that sanitizes commands in AI-generated workflow JSON.
///
/// Wraps `sanitize_commands_in_steps()` to fix jq commands, Python f-string
/// escaping, nested retry format, curl -f in pipes, etc.
pub struct CommandSanitizer;

impl AiMiddleware for CommandSanitizer {
    fn name(&self) -> &'static str {
        "command-sanitizer"
    }

    fn post_call(&self, response: &mut AiResponse, _ctx: &MiddlewareContext) {
        if !response.success {
            return;
        }

        // Try to parse the response as a workflow and sanitize commands
        let json_text =
            crate::workflow_generation::generator::extract_json_from_response(&response.output);
        if let Ok(mut workflow) =
            serde_json::from_str::<crate::unified_workflows::UnifiedWorkflow>(&json_text)
        {
            let mut total = 0;
            total += sanitize_commands_in_steps(&mut workflow.setup_steps);
            total += sanitize_commands_in_steps(&mut workflow.verification_steps);
            total += sanitize_commands_in_steps(&mut workflow.completion_steps);
            for stage in workflow.stages.iter_mut() {
                total += sanitize_commands_in_steps(&mut stage.setup_steps);
                total += sanitize_commands_in_steps(&mut stage.verification_steps);
                total += sanitize_commands_in_steps(&mut stage.completion_steps);
            }
            if total > 0 {
                info!("CommandSanitizer middleware: fixed {} steps", total);
                if let Ok(fixed_json) = serde_json::to_string_pretty(&workflow) {
                    response.output = fixed_json;
                }
            }
        }
    }
}

/// Post-call middleware that fixes SDK URLs in AI-generated workflow JSON.
///
/// Wraps `fix_sdk_urls()` to replace `/control/` with `/sdk/` URLs and
/// inject missing SDK connect steps.
pub struct SdkUrlSanitizer;

impl AiMiddleware for SdkUrlSanitizer {
    fn name(&self) -> &'static str {
        "sdk-url-sanitizer"
    }

    fn post_call(&self, response: &mut AiResponse, _ctx: &MiddlewareContext) {
        if !response.success {
            return;
        }

        let json_text =
            crate::workflow_generation::generator::extract_json_from_response(&response.output);
        if let Ok(workflow) =
            serde_json::from_str::<crate::unified_workflows::UnifiedWorkflow>(&json_text)
        {
            let fixed = fix_sdk_urls(&workflow);
            if let Ok(fixed_json) = serde_json::to_string_pretty(&fixed) {
                if fixed_json != response.output {
                    info!("SdkUrlSanitizer middleware: fixed SDK URLs in AI output");
                    response.output = fixed_json;
                }
            }
        }
    }
}

/// Build the standard hardener middleware chain with all sanitizers.
///
/// This chain is applied around the hardener AI call to catch issues
/// both before sending to the AI and after receiving the response.
///
/// `StructuredOutputMiddleware` is added last so that its `post_call`
/// (which runs first in reverse order) validates the response against
/// the expected JSON schema before other post-processing transforms.
pub fn build_hardener_middleware() -> AiMiddlewareChain {
    AiMiddlewareChain::new()
        .add(CommandSanitizer)
        .add(SdkUrlSanitizer)
        .add(crate::ai_provider::middleware::StructuredOutputMiddleware)
}

/// Deterministic fixup: replace `/ui-bridge/control/` with `/ui-bridge/sdk/` in command steps
/// when an SDK connect step is present. Also injects a missing SDK connect step if the workflow
/// targets a web app but doesn't have one.
///
/// This catches a common AI generation mistake where the builder agent uses the runner's
/// own control endpoint instead of the SDK proxy endpoint.
pub fn fix_sdk_urls(workflow: &UnifiedWorkflow) -> UnifiedWorkflow {
    let workflow_json = serde_json::to_string(workflow).unwrap_or_default();
    let has_sdk_connect = workflow_json.contains("ui-bridge/sdk/connect");
    let targets_web =
        workflow_json.contains("localhost:3001") || workflow_json.contains("localhost:1420");
    let has_control_urls = workflow_json.contains("ui-bridge/control/");
    // Also detect SDK URLs that need a connect step (AI may generate /sdk/ directly)
    let has_sdk_urls = workflow_json.contains("ui-bridge/sdk/snapshot")
        || workflow_json.contains("ui-bridge/sdk/elements")
        || workflow_json.contains("ui-bridge/sdk/ai/")
        || workflow_json.contains("ui-bridge/sdk/discover")
        || workflow_json.contains("ui-bridge/sdk/page/navigate");

    // Nothing to fix
    if !has_control_urls && !has_sdk_urls && !targets_web {
        return workflow.clone();
    }
    if !has_control_urls && has_sdk_connect {
        return workflow.clone();
    }

    let mut fixed = workflow.clone();
    let mut fixup_count = 0;

    // If targeting web app but missing SDK connect, inject one at the start of setup.
    // This handles both cases: AI generated /control/ URLs (to be fixed below) or
    // AI correctly generated /sdk/ URLs but forgot the connect step.
    if targets_web && !has_sdk_connect && (has_control_urls || has_sdk_urls) {
        // Determine target URL from the workflow
        let target_url = if workflow_json.contains("localhost:3001") {
            "http://localhost:3001"
        } else {
            "http://localhost:1420"
        };

        let connect_step = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "command",
            "phase": "setup",
            "mode": "shell",
            "name": "Connect UI Bridge SDK",
            "command": format!(
                "curl -s -X POST http://localhost:9876/ui-bridge/sdk/connect -H \"Content-Type: application/json\" -d '{{\"url\": \"{}\"}}'",
                target_url
            ),
            "fail_on_error": true
        });
        fixed.setup_steps.insert(0, connect_step);
        fixup_count += 1;
        info!(
            "SDK URL fixup: injected missing SDK connect step for {}",
            target_url
        );
    }

    // Replace /ui-bridge/control/ with /ui-bridge/sdk/ in all command steps
    let fix_steps = |steps: &mut Vec<Value>| -> usize {
        let mut count = 0;
        for step in steps.iter_mut() {
            let changed = fix_control_urls_in_step(step);
            if changed {
                count += 1;
            }
        }
        count
    };

    fixup_count += fix_steps(&mut fixed.setup_steps);
    fixup_count += fix_steps(&mut fixed.verification_steps);
    fixup_count += fix_steps(&mut fixed.completion_steps);

    // Fix SDK URLs in stage steps
    for stage in fixed.stages.iter_mut() {
        fixup_count += fix_steps(&mut stage.setup_steps);
        fixup_count += fix_steps(&mut stage.verification_steps);
        fixup_count += fix_steps(&mut stage.completion_steps);
    }

    // Add retry parameters to SDK verification steps after navigation
    let mut retry_count = inject_retries_after_navigation(&mut fixed.verification_steps);
    for stage in fixed.stages.iter_mut() {
        retry_count += inject_retries_after_navigation(&mut stage.verification_steps);
    }
    if retry_count > 0 {
        info!(
            "SDK URL fixup: added retries to {} verification steps after navigation",
            retry_count
        );
    }

    if fixup_count > 0 {
        info!(
            "SDK URL fixup: corrected {} steps from /control/ to /sdk/ paths",
            fixup_count
        );
    }

    fixed
}

/// Fix `/ui-bridge/control/` URLs to `/ui-bridge/sdk/` in a single step's command fields.
/// Returns true if any changes were made.
fn fix_control_urls_in_step(step: &mut Value) -> bool {
    let mut changed = false;

    // Fields that may contain URLs (check both "command" and "shell_command" variants)
    for field in &["command", "shell_command", "check_url", "check_command"] {
        if let Some(val) = step.get_mut(*field) {
            if let Some(s) = val.as_str() {
                if s.contains("ui-bridge/control/") {
                    let fixed = s.replace("ui-bridge/control/", "ui-bridge/sdk/");
                    *val = Value::String(fixed);
                    changed = true;
                }
            }
        }
    }

    changed
}

/// Add retry parameters to SDK verification steps that follow a navigation step.
///
/// After page navigation, the WebSocket connection needs ~15s to reconnect.
/// This function adds `retry_count: 5` and `retry_delay_ms: 3000` to SDK-related
/// command steps that come after a navigation step in verification.
fn inject_retries_after_navigation(steps: &mut [Value]) -> usize {
    let mut count = 0;
    let mut after_nav = false;

    for step in steps.iter_mut() {
        let cmd = step
            .get("command")
            .or_else(|| step.get("shell_command"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Detect navigation steps
        if cmd.contains("sdk/page/navigate") {
            after_nav = true;
            continue;
        }

        // Add retries to SDK steps after navigation (if they don't already have them)
        if after_nav && cmd.contains("ui-bridge/sdk/") && step.get("retry_count").is_none() {
            if let Some(obj) = step.as_object_mut() {
                obj.insert("retry_count".to_string(), Value::Number(5.into()));
                obj.insert("retry_delay_ms".to_string(), Value::Number(3000.into()));
            }
            count += 1;
        }

        // Stop adding retries after we hit a non-SDK step (like a typecheck)
        if after_nav && !cmd.contains("ui-bridge/sdk/") && !cmd.is_empty() {
            after_nav = false;
        }
    }

    count
}

/// Sanitize commands in workflow steps to fix common issues:
/// 1. Replaces `jq` commands with Python equivalents (jq unavailable on Windows)
/// 2. Fixes Python f-string escaping issues
/// 3. Normalizes nested `retry` objects to flat `retry_count`/`retry_delay_ms` fields
/// 4. Removes `curl -f` flag in piped commands (suppresses output on HTTP errors)
pub fn sanitize_commands_in_steps(steps: &mut [Value]) -> usize {
    let mut count = 0;

    for step in steps.iter_mut() {
        // Fix nested retry format: {"retry": {"count": N, "delay_ms": M}} -> retry_count/retry_delay_ms
        if let Some(retry_obj) = step.get("retry").cloned() {
            if let Some(obj) = step.as_object_mut() {
                if let Some(c) = retry_obj.get("count").and_then(|v| v.as_u64()) {
                    obj.insert("retry_count".to_string(), Value::Number(c.into()));
                }
                if let Some(d) = retry_obj.get("delay_ms").and_then(|v| v.as_u64()) {
                    obj.insert("retry_delay_ms".to_string(), Value::Number(d.into()));
                }
                obj.remove("retry");
                count += 1;
            }
        }

        // Check both "command" and "shell_command" keys — workflows use "command" during
        // generation but "shell_command" after deserialization through ExecutionStepConfig.
        let (cmd_key, cmd) = match step.get("command").and_then(|v| v.as_str()) {
            Some(c) => ("command", c.to_string()),
            None => match step.get("shell_command").and_then(|v| v.as_str()) {
                Some(c) => ("shell_command", c.to_string()),
                None => continue,
            },
        };

        // Replace jq commands with python -c equivalents (jq unavailable on Windows MSYS)
        if cmd.contains("| jq ") {
            if let Some(fixed) = replace_jq_with_python(&cmd) {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert(cmd_key.to_string(), Value::String(fixed));
                    count += 1;
                }
                continue;
            }
        }

        // Fix f-string escaping issues in python commands → replace with clean python
        if cmd.contains("python -c")
            && cmd.contains("json.load")
            && (cmd.contains("f'") || cmd.contains("f\""))
        {
            if let Some(fixed) = replace_python_fstring_with_clean(&cmd) {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert(cmd_key.to_string(), Value::String(fixed));
                    count += 1;
                }
            }
        }

        // Replace bash negation prefix `! command` with explicit exit code handling.
        // `! grep ...` is bash-specific; the shell router may not detect `!` as bash syntax,
        // causing it to be routed to cmd.exe where it fails.
        // Fix: `! grep -qE 'pat' file` → `grep -qE 'pat' file && exit 1 || exit 0`
        let current_cmd_for_negation = step.get(cmd_key).and_then(|v| v.as_str()).unwrap_or(&cmd);
        if current_cmd_for_negation.trim().starts_with("! ") {
            let inner_cmd = current_cmd_for_negation.trim().strip_prefix("! ").unwrap();
            let fixed = format!("{} && exit 1 || exit 0", inner_cmd);
            if let Some(obj) = step.as_object_mut() {
                obj.insert(cmd_key.to_string(), Value::String(fixed));
                count += 1;
            }
        }

        // Fix `curl -sf` (or `-sف`) in piped commands: the -f flag suppresses ALL output
        // on HTTP errors, so the downstream command (python, grep) receives empty stdin
        // and fails with a confusing error. Replace with `curl -s` when output is piped.
        let current_cmd_for_curl = step.get(cmd_key).and_then(|v| v.as_str()).unwrap_or(&cmd);
        if current_cmd_for_curl.contains("| ") {
            let fixed_curl = fix_curl_sf_in_piped_commands(current_cmd_for_curl);
            if fixed_curl != current_cmd_for_curl {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert(cmd_key.to_string(), Value::String(fixed_curl));
                    count += 1;
                }
            }
        }

        // Fix incorrect health check assertions: d.get('status')=='ok' → d.get('success')==True
        // The runner API returns {"success": true, "data": "ok"}, not {"status": "ok"}.
        // AI models frequently generate the wrong assertion despite schema_context documentation.
        let current_cmd_for_health = step.get(cmd_key).and_then(|v| v.as_str()).unwrap_or(&cmd);
        if current_cmd_for_health.contains("/health")
            && current_cmd_for_health.contains("python")
            && current_cmd_for_health.contains("d.get('status')")
        {
            let fixed_health = current_cmd_for_health.replace(
                "d.get('status')=='ok'",
                "d.get('success')==True and d.get('data')=='ok'",
            );
            if fixed_health != current_cmd_for_health {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert(cmd_key.to_string(), Value::String(fixed_health));
                    count += 1;
                    info!("Fixed incorrect health check assertion: status → success/data");
                }
            }
        }

        // Quote URLs containing & in curl commands — unquoted & is misinterpreted
        // as a shell command separator by bash, e.g.:
        //   curl http://host/api?a=1&b=2 | grep x
        // bash parses as: (curl http://host/api?a=1) & (b=2 | grep x)
        // Fix: wrap the URL in double quotes so & is treated as literal
        let current_cmd = step.get(cmd_key).and_then(|v| v.as_str()).unwrap_or(&cmd);
        if let Some(fixed) = quote_curl_urls_with_ampersand(current_cmd) {
            if let Some(obj) = step.as_object_mut() {
                obj.insert(cmd_key.to_string(), Value::String(fixed));
                count += 1;
            }
        }
    }

    count
}

/// Replace `jq` commands with Python equivalents since jq is not available on Windows MSYS.
///
/// Handles patterns like:
/// - `curl ... | jq -e '.elements | length > N'` → `curl ... | python -c "import sys,json; ..."`
/// - `curl ... | jq -e '.total > N'` → `curl ... | python -c "import sys,json; ..."`
/// - `curl ... | jq -e '.data | length > N'` → `curl ... | python -c "import sys,json; ..."`
///
/// Note: UI Bridge SDK `/elements` endpoint returns `{"data": [...]}` (not `{"elements": [...], "total": N}`).
/// The `.total` pattern is mapped to `len(data)` for SDK element endpoints.
fn replace_jq_with_python(cmd: &str) -> Option<String> {
    let pipe_idx = cmd.find("| jq ")?;
    let curl_part = cmd[..pipe_idx].trim();
    let jq_part = &cmd[pipe_idx + 5..]; // skip "| jq "

    // Extract the jq expression (may be quoted with -e flag)
    let jq_expr = jq_part
        .trim()
        .trim_start_matches("-e ")
        .trim()
        .trim_matches('"')
        .trim_matches('\'');

    // Detect if this is an SDK elements endpoint (returns {data: [...]} not {elements: [...]})
    let is_sdk_elements = curl_part.contains("ui-bridge/sdk/elements");

    // Pattern: .data | length > N (SDK elements response format)
    if jq_expr.contains("data") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} items, got '+str(len(items))\"",
            curl_part, threshold, threshold
        ));
    }

    // Pattern: .elements | length > N
    if jq_expr.contains("elements") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        if is_sdk_elements {
            // SDK /elements returns {data: [...]} not {elements: [...]}
            return Some(format!(
                "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('data',[]); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
                curl_part, threshold, threshold
            ));
        }
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('elements',d.get('data',[])); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
            curl_part, threshold, threshold
        ));
    }

    // Pattern: .total > N
    if jq_expr.contains("total") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        if is_sdk_elements {
            // SDK /elements returns {data: [...]} — use len(data) instead of nonexistent total field
            return Some(format!(
                "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} elements, got '+str(len(items))\"",
                curl_part, threshold, threshold
            ));
        }
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); t=d.get('total',len(d.get('data',[]))); assert t>{}, 'Expected total>{}, got '+str(t)\"",
            curl_part, threshold, threshold
        ));
    }

    // Pattern: .results | length > N
    if jq_expr.contains("results") && jq_expr.contains("length") {
        let threshold = extract_number_threshold(jq_expr).unwrap_or(0);
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); r=d.get('results',[]); assert len(r)>{}, 'Expected >{} results, got '+str(len(r))\"",
            curl_part, threshold, threshold
        ));
    }

    None
}

/// Replace a `python -c` command with f-strings with a clean version without f-strings.
fn replace_python_fstring_with_clean(cmd: &str) -> Option<String> {
    let pipe_idx = cmd.find("| python")?;
    let curl_part = cmd[..pipe_idx].trim();
    let python_part = &cmd[pipe_idx..];

    // Detect if this is an SDK elements endpoint (returns {data: [...]} not {elements: [...]})
    let is_sdk_elements = curl_part.contains("ui-bridge/sdk/elements");

    // Detect element count assertions
    if python_part.contains("elements") && python_part.contains("len(") {
        let threshold = extract_number_threshold(python_part).unwrap_or(0);
        let key = if is_sdk_elements { "data" } else { "elements" };
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); elems=d.get('{}',d.get('data',[])); assert len(elems)>{}, 'Expected >{} elements, got '+str(len(elems))\"",
            curl_part, key, threshold, threshold
        ));
    }

    // Detect total assertions
    if python_part.contains("total") {
        let threshold = extract_number_threshold(python_part).unwrap_or(0);
        if is_sdk_elements {
            // SDK /elements returns {data: [...]} — use len(data) instead of nonexistent total
            return Some(format!(
                "{} | python -c \"import sys,json; d=json.load(sys.stdin); items=d.get('data',[]); assert len(items)>{}, 'Expected >{} elements, got '+str(len(items))\"",
                curl_part, threshold, threshold
            ));
        }
        return Some(format!(
            "{} | python -c \"import sys,json; d=json.load(sys.stdin); t=d.get('total',len(d.get('data',[]))); assert t>{}, 'Expected total>{}, got '+str(t)\"",
            curl_part, threshold, threshold
        ));
    }

    None
}

/// Fix `curl -sf` in piped commands by removing the `-f` flag.
///
/// The `-f` (fail) flag makes curl suppress ALL output on HTTP error status codes (4xx/5xx).
/// When the output is piped to another command (e.g., `python -c`, `grep`), the downstream
/// command receives empty stdin and fails with a confusing error like `JSONDecodeError`.
///
/// This function removes `-f` from curl flags when the command pipes to another program.
fn fix_curl_sf_in_piped_commands(cmd: &str) -> String {
    if !cmd.contains("curl ") || !cmd.contains("| ") {
        return cmd.to_string();
    }

    // Match patterns: "curl -sf", "curl -fs", "curl -sfL", "curl -fsL", etc.
    // We need to remove just the 'f' from the flags group.
    let mut result = cmd.to_string();
    let mut changed = false;

    // Find curl invocations and their flag groups
    for (i, _) in cmd.match_indices("curl ") {
        let after_curl = &cmd[i + 5..];
        // Look for a flags group starting with -
        if let Some(flags_start) = after_curl.find('-') {
            let flags_abs = i + 5 + flags_start;
            // Extract the flag group (everything until next space)
            let flags_end = after_curl[flags_start..]
                .find(' ')
                .map(|p| flags_abs + p)
                .unwrap_or(cmd.len());
            let flags = &cmd[flags_abs..flags_end];

            // Only process single-letter flag groups (like -sf, -sfL, -fsL)
            // Skip long flags like --fail
            if flags.starts_with('-') && !flags.starts_with("--") && flags.contains('f') {
                let new_flags: String = flags.chars().filter(|c| *c != 'f').collect();
                if new_flags == "-" {
                    // Only had -f, replace with -s if not already present
                    result = format!("{}-s{}", &result[..flags_abs], &result[flags_end..]);
                } else {
                    result = format!(
                        "{}{}{}",
                        &result[..flags_abs],
                        new_flags,
                        &result[flags_end..]
                    );
                }
                changed = true;
                break; // Only fix the first curl invocation
            }
        }
    }

    if changed {
        result
    } else {
        cmd.to_string()
    }
}

/// Quote URLs containing `&` in curl commands so bash doesn't interpret `&` as a
/// background operator / command separator.
///
/// For example:
/// ```text
/// curl -sf http://host/api?a=1&b=2 | grep x
/// ```
/// becomes:
/// ```text
/// curl -sf "http://host/api?a=1&b=2" | grep x
/// ```
///
/// Returns `None` if no fix is needed (no unquoted URLs with `&`).
fn quote_curl_urls_with_ampersand(cmd: &str) -> Option<String> {
    if !cmd.contains("curl ") || !cmd.contains('&') {
        return None;
    }

    // Find URL-like tokens in the command: http:// or https:// followed by non-whitespace
    // that contain both ? and & (multi-param query strings)
    let mut result = cmd.to_string();
    let mut changed = false;

    // Scan for http(s):// URLs that are NOT already quoted
    let url_prefixes = ["http://", "https://"];
    for prefix in &url_prefixes {
        let mut search_from = 0;
        loop {
            let Some(url_start) = result[search_from..].find(prefix).map(|i| i + search_from)
            else {
                break;
            };

            // Check if URL is already quoted (preceded by " or ')
            if url_start > 0 {
                let prev_char = result.as_bytes()[url_start - 1];
                if prev_char == b'"' || prev_char == b'\'' {
                    search_from = url_start + prefix.len();
                    continue;
                }
            }

            // Find end of URL (next whitespace, pipe, or end of string)
            let url_end = result[url_start..]
                .find(|c: char| c.is_whitespace() || c == '|' || c == ';' || c == ')' || c == '\'')
                .map(|i| i + url_start)
                .unwrap_or(result.len());

            let url = &result[url_start..url_end];

            // Only fix if URL has query params with & (the problematic case)
            if url.contains('?') && url.contains('&') {
                let quoted = format!("\"{}\"", url);
                result = format!("{}{}{}", &result[..url_start], quoted, &result[url_end..]);
                changed = true;
                search_from = url_start + quoted.len();
            } else {
                search_from = url_end;
            }
        }
    }

    if changed {
        Some(result)
    } else {
        None
    }
}

/// Extract a numeric threshold from an assertion string like "len(...)>2" or "... > 0"
fn extract_number_threshold(s: &str) -> Option<u32> {
    // Look for patterns like ">2", "> 2", ">0", "> 0"
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'>' && i + 1 < bytes.len() {
            let rest = &s[i + 1..].trim_start();
            if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
                if let Ok(n) = rest[..end].parse() {
                    return Some(n);
                }
            } else if let Ok(n) = rest.parse() {
                return Some(n);
            }
        }
    }
    None
}

/// Count verification steps that are candidates for hardening.
fn count_candidates(workflow: &UnifiedWorkflow) -> usize {
    let has_sdk = {
        let json = serde_json::to_string(workflow).unwrap_or_default();
        json.contains("ui-bridge/sdk/connect")
    };

    let is_candidate = |step: &&Value| -> bool {
        let t = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mode = step.get("mode").and_then(|v| v.as_str()).unwrap_or("");
        match t {
            "prompt" => true,
            "command" if (mode == "test" || step.get("test_type").is_some()) && has_sdk => {
                let tt = step.get("test_type").and_then(|v| v.as_str()).unwrap_or("");
                tt == "playwright" || is_ui_verification_test(step)
            }
            "command" if mode == "shell" && has_sdk => {
                let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
                cmd.contains("ui-bridge/sdk") && !cmd.contains("grep") && !cmd.contains("jq")
            }
            _ => false,
        }
    };

    let top_level = workflow
        .verification_steps
        .iter()
        .filter(is_candidate)
        .count();
    let stage_level: usize = workflow
        .stages
        .iter()
        .map(|stage| stage.verification_steps.iter().filter(is_candidate).count())
        .sum();

    top_level + stage_level
}

// ============================================================================
// Prompt builder
// ============================================================================

fn build_hardener_prompt(
    workflow_json: &str,
    description: &str,
    app_context: &AppContext,
    pg_db: Option<&Arc<PgDb>>,
    skill_registry: &SkillRegistry,
    insights_section: Option<&str>,
    tool_tags: Option<&[String]>,
    constitution: Option<&str>,
) -> String {
    let mut prompt = format!(
        r#"You are a verification hardener agent for Qontinui Runner.

Your job is to analyze ALL verification steps and convert them to the best available deterministic
approach. The workflow below was generated for this task: "{description}"

## Conversion Rules
"#,
        description = description,
    );

    // Load conversion rules from PG or use fallback
    let conversion_rules = if let Some(pg) = pg_db {
        let pg_clone = pg.clone();
        let all = tokio::runtime::Handle::current().block_on(async {
            pg_clone.get_active_rules("hardener", Some("conversion_rules")).await.unwrap_or_default()
        });
        if !all.is_empty() {
            // Filter by condition
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|r| match r.condition.as_deref() {
                    None => true,
                    Some("has_sdk_connect") => app_context.has_sdk_connect,
                    Some("targets_web_app") => app_context.targets_web_app,
                    _ => true,
                })
                .collect();
            Some(filtered)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(ref db_rules) = conversion_rules {
        for rule in db_rules {
            prompt.push_str(&format!(
                "\n### Rule {}: {}\n\n{}\n",
                rule.rule_number, rule.title, rule.content
            ));
        }
    } else {
        // Fallback: hardcoded conversion rules
        prompt.push_str(
            r#"
### Rule 1: Convert `prompt` steps to deterministic equivalents

Only 3 step types are valid: `command`, `ui_bridge`, and `prompt`. Convert prompt steps to the appropriate type.

| Prompt check type | Convert to | Method |
|---|---|---|
| UI element presence/structure | `command` | curl to UI Bridge SDK endpoint, pipe to grep for content check |
| Content/text on page | `command` | curl to UI Bridge SDK `/ai/search`, pipe to grep for expected text |
| File existence | `command` | `check_type: "custom_command"` with `test -f <path>` |
| File content | `command` | `check_type: "custom_command"` with `grep -q <pattern> <file>` |
| Code quality (lint) | `command` | `check_type: "lint"` with appropriate command |
| Code quality (typecheck) | `command` | `check_type: "typecheck"` with appropriate command |
| API health/response | `command` | curl to endpoint, check exit code |
| UI assertion | `ui_bridge` | Use assert action with target and expected value |
| Subjective/qualitative | Keep as `prompt` | Cannot be made deterministic |
"#,
        );

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 2: Replace Playwright UI tests with UI Bridge SDK checks

When the UI Bridge SDK is connected (it IS connected in this workflow's setup), Playwright-based UI
verification tests should be converted to `command` steps (using curl) or `ui_bridge` steps. The SDK provides
direct programmatic access to registered UI elements without requiring a Playwright browser instance.

**Why:** Playwright tests require browser binaries (often missing), are slow, flaky, and test through
the browser rendering layer. The UI Bridge SDK communicates directly with the app's element registry,
making checks faster, more reliable, and always available.

**How to convert a Playwright test step to SDK-based steps:**

If a single Playwright test checks multiple things (e.g., "verify tabs exist AND thumbnails render AND
buttons work"), split it into multiple `command` or `ui_bridge` steps — one per distinct verification concern.
Each step should check one thing well rather than trying to replicate the entire test.

Common conversions:
| Playwright test checks... | SDK equivalent |
|---|---|
| Element exists on page | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true \| grep "elementId"` |
| Text content visible | `command`: `curl -sf -X POST http://localhost:9876/ui-bridge/sdk/ai/search -H "Content-Type: application/json" -d '{"text":"..."}' \| grep "expected-text"` |
| Tab/section present | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true \| grep "tab-name"` |
| Page loads without errors | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/snapshot \| grep "elements"` |
| Element state assertion | `ui_bridge`: Use assert action with target element and expected value |
| Element count/state | `command`: `curl -sf http://localhost:9876/ui-bridge/sdk/elements \| grep "expected-element"` |

**URL base for all SDK endpoints:** `http://localhost:9876/ui-bridge/sdk/`

Keep the original step's `id` on ONE of the replacement steps, and generate new UUIDs for additional steps.

**IMPORTANT — SDK capability limits.** Do NOT convert Playwright tests that involve any of these
interactions, because the UI Bridge SDK cannot perform them:
- **Keyboard shortcuts** (Ctrl+Z, Delete, Ctrl+S, etc.)
- **File upload dialogs**
- **Form submit/reset** (not implemented in SDK)
- **Multi-tab or multi-window** interactions
- **Screenshot pixel comparisons**

These tests MUST remain as `command` steps with `test_type: "playwright"`.

Note: **Drag-and-drop CAN be converted** to SDK. Use `POST /ui-bridge/sdk/ai/execute` with
`{"action":"drag","elementId":"<source>","params":{"target":{"elementId":"<target>"}}}` or
with `{"action":"drag","elementId":"<source>","params":{"targetPosition":{"x":N,"y":N}}}`.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 3: Strengthen weak SDK verification commands

If an existing `command` step calls a UI Bridge SDK endpoint via curl but only checks exit code (no grep),
add a pipe to `grep` to verify meaningful content. A successful curl to the SDK just means the
endpoint is reachable — it doesn't verify the UI state. SDK endpoints return 200 even for EMPTY results.

**Specific endpoint guidance:**

- **`POST /ui-bridge/sdk/ai/search`**: The response is `{"results": [...], "total": N}`.
  When `total` is 0, the search found NOTHING — but curl still succeeds!
  **FIX:** Pipe to `grep` with the expected element text (e.g., `| grep "Settings"`).

- **`GET /ui-bridge/sdk/elements`** or **`GET /ui-bridge/sdk/elements?contentOnly=true`**:
  Pipe to `grep "id"` at minimum (verifies elements exist). Better: grep for the expected element ID or text.

- **`GET /ui-bridge/sdk/snapshot`**: Pipe to `grep "elements"` (verifies snapshot has data).
  Better: grep for the expected page title or a known element's text content.

- **`POST /ui-bridge/sdk/discover`**: Pipe to `grep` with expected element attributes or text.

- **`GET /ui-bridge/sdk/ai/summary`** or **`GET /ui-bridge/sdk/ai/snapshot`**:
  Pipe to `grep` with a keyword from the expected page content.
"#);
        }

        if app_context.has_sdk_connect {
            prompt.push_str(r#"
### Rule 4: Inject page navigation before SDK verification checks

If the workflow's setup_steps include a page navigation step (curl POST to `/ui-bridge/sdk/page/navigate` or
a `ui_bridge` step with `action: "navigate"`), the verification phase MUST also navigate to that same URL
before any SDK element checks. This is because the agentic phase may navigate the browser away from the
target page (e.g., to documentation, error pages, or other app routes), and verification needs to be on the
correct page to check UI state.

**How to apply:**
1. Look in `setup_steps` for any step that navigates to a URL (curl to `/ui-bridge/sdk/page/navigate` or `ui_bridge` navigate action)
2. Extract the target URL
3. If the FIRST SDK-related step in `verification_steps` is NOT a navigation step, INSERT a new `command`
   step at the beginning of verification_steps:
   ```json
   {
     "id": "<new-uuid>",
     "type": "command",
     "phase": "verification",
     "name": "Navigate to target page",
     "command": "curl -sf -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -H \"Content-Type: application/json\" -d \"{\\\"url\\\": \\\"<extracted-url>\\\"}\"",
     "fail_on_error": true
   }
   ```
   Or use a `ui_bridge` step with `action: "navigate"` and the target URL.
4. If verification already starts with a navigate step to the same URL, skip this rule.
"#);
        }

        prompt.push_str(r#"
### Rule 5: Ensure every agentic step has corresponding verification coverage

Examine EACH prompt step in `agentic_steps` and identify the distinct goals/features it describes.
Then check whether `verification_steps` has at least one deterministic step (`command` or `ui_bridge`)
that would FAIL if that specific goal was NOT implemented.

**Common gaps:**
- Agentic step says "implement drag-and-drop" but verification only checks tab existence
- Agentic step says "add thumbnails to state nodes" but no verification checks for thumbnails
- Agentic step says "add keyboard shortcuts" but no verification for keyboard functionality

**How to fix gaps:**
1. For each uncovered agentic goal, ADD a new `command` verification step (e.g., curl to SDK endpoint
   piped to grep for expected content) or `ui_bridge` step with assert action
2. Generate new UUIDs for added steps
3. If an agentic step has multiple distinct goals (e.g., "implement thumbnails AND drag-and-drop"),
   add one verification step per goal

**Tab/section existence is NOT adequate coverage.** Checking that a tab named "State View" exists does NOT
verify that the tab has spatial visualization content. Add content-specific checks.
"#);
    }

    // Add context-specific guidance
    if app_context.targets_web_app {
        prompt.push_str("\n### Web App Context\n");
        prompt.push_str("This workflow targets a web application. ");
        if app_context.has_sdk_connect {
            prompt.push_str("UI Bridge SDK is connected in setup. Use SDK endpoints:\n");
            prompt.push_str("- Element list: `GET http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true`\n");
            prompt.push_str("- AI search: `POST http://localhost:9876/ui-bridge/sdk/ai/search` with body `{\"text\":\"query\"}`\n");
            prompt
                .push_str("- Page snapshot: `GET http://localhost:9876/ui-bridge/sdk/snapshot`\n");
            prompt.push_str("- Execute action: `POST http://localhost:9876/ui-bridge/sdk/ai/execute` with body `{\"action\":\"click\",\"elementId\":\"id\"}`\n");
            if let Some(ref nav_url) = app_context.setup_navigate_url {
                prompt.push_str(&format!(
                    "- Setup navigates to: `{}` — verification should navigate here before SDK checks (Rule 4)\n",
                    nav_url
                ));
            }
        } else {
            prompt
                .push_str("No SDK connect step found — prefer file-based or API health checks.\n");
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

    // Load critical rules from PG or use fallback
    let critical_section = if let Some(pg) = pg_db {
        let pg_clone = pg.clone();
        let critical = tokio::runtime::Handle::current().block_on(async {
            pg_clone.get_active_rules("hardener", Some("critical_rules")).await.unwrap_or_default()
        });
        if !critical.is_empty() {
            let mut s = String::from("\n## Critical Rules\n\n");
            s.push_str(&rules::format_rules_as_markdown(&critical));
            Some(s)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(critical) = critical_section {
        prompt.push_str(&critical);
    } else {
        prompt.push_str(r#"
## Critical Rules

1. **Only modify verification_steps**: Do NOT change setup_steps, agentic_steps, or completion_steps
2. **Preserve step IDs**: Every step must keep its original `id` field (unless splitting a step, in which case keep the original ID on one and generate new UUIDs for additions)
3. **Preserve criterion_id**: If a verification step has a `criterion_id` field, keep it on the converted step (or on the primary replacement step if splitting). Do NOT modify, replace, or remove `criterion_id` values — they link verification steps to acceptance criteria.
4. **Preserve step order**: Steps must remain in the same relative position
5. **Adding steps is allowed**: If a Playwright test step checks multiple things, you MAY replace it with multiple `command` or `ui_bridge` steps. You MAY also add NEW verification steps to cover uncovered agentic goals. Keep original `id`s on existing steps and generate new UUIDs for additions.
6. **Keep subjective prompts**: If a prompt step is genuinely subjective (e.g., "Is the UX intuitive?"), keep it as `prompt`
7. **Complete required fields**: Every converted step must have all required fields for its new type
8. **Only 3 step types**: All steps must use `command`, `ui_bridge`, or `prompt`. Do NOT output `api_request`, `check`, `test`, `gate`, or `spec` types.
9. **Command with check_type fields**: For check conversions, include `mode: "check"`, `check_type`, `command`, and `working_directory` on the `command` step
10. **Do not convert existing command+check_type steps**: Do NOT convert `command` steps that already have `check_type` set (lint, typecheck, etc.) — they are already deterministic
11. **SDK verification uses command+curl**: Use `command` steps with `mode: "shell"` and curl piped to grep for SDK-based verification, not `api_request`
12. **Always set mode on command steps**: Every `command` step must include a `mode` field (`shell`, `check`, `check_group`, or `test`) matching the fields present
13. **No bash negation prefix**: NEVER use `!` as a command prefix to invert exit codes (e.g., `! grep -qE 'pattern' file`). The `!` operator is bash-specific and may not be detected by the shell router on Windows. Instead, use: `grep -qE 'pattern' file && exit 1 || exit 0`
14. **Regression steps**: Steps with a `regression_issue_id` field or IDs starting with `regression-` must be preserved EXACTLY as-is. Do not modify, remove, or attempt to harden these steps. They are deterministic regression checks tied to specific issues and must remain unchanged."#);
    }

    // Include skill catalog context so the hardener can match steps to known skills
    let skills_section = match tool_tags {
        Some(tags) if !tags.is_empty() => {
            format_skills_for_generator_filtered(skill_registry, None, tags, None)
        }
        _ => format_skills_for_generator(skill_registry),
    };
    if !skills_section.is_empty() {
        prompt.push_str(&format!(
            "\n\n{}\nWhen hardening steps, prefer configurations that align with these known skill templates. If a verification step matches a skill's purpose, use the equivalent deterministic configuration from the skill catalog.\n",
            skills_section
        ));
    }

    if let Some(insights) = insights_section {
        if !insights.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(insights);
        }
    }

    // Inject project constitution so hardened steps respect project constraints
    if let Some(constitution_text) = constitution {
        prompt.push_str("\n\n");
        prompt.push_str(&crate::workflow_generation::constitution::format_constitution_for_prompt(constitution_text));
        prompt.push_str("Hardened steps MUST comply with the constitution. Do NOT introduce commands or patterns that violate these constraints.\n");
    }

    prompt.push_str(&format!(
        r#"

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
    // Step count may increase (splitting is allowed) but never decrease
    if hardened.verification_steps.len() < original.verification_steps.len() {
        return Some(format!(
            "Step count decreased: {} -> {} (splitting adds steps, never removes)",
            original.verification_steps.len(),
            hardened.verification_steps.len()
        ));
    }

    // All original step IDs must still be present (in any order since splits insert after)
    for orig_step in &original.verification_steps {
        let orig_id = orig_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if orig_id.is_empty() {
            continue;
        }
        let found = hardened.verification_steps.iter().any(|h| {
            h.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id == orig_id)
                .unwrap_or(false)
        });
        if !found {
            return Some(format!(
                "Original step ID '{}' is missing from hardened output",
                orig_id
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

    // Validate stage count is preserved
    if original.stages.len() != hardened.stages.len() {
        return Some(format!(
            "Stage count changed: {} -> {}",
            original.stages.len(),
            hardened.stages.len()
        ));
    }

    // Validate per-stage invariants
    for (idx, (orig_stage, hard_stage)) in original
        .stages
        .iter()
        .zip(hardened.stages.iter())
        .enumerate()
    {
        // Stage verification step count may increase but not decrease
        if hard_stage.verification_steps.len() < orig_stage.verification_steps.len() {
            return Some(format!(
                "stages[{}] verification step count decreased: {} -> {}",
                idx,
                orig_stage.verification_steps.len(),
                hard_stage.verification_steps.len()
            ));
        }

        // All original stage verification step IDs must be present
        for orig_step in &orig_stage.verification_steps {
            let orig_id = orig_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if orig_id.is_empty() {
                continue;
            }
            let found = hard_stage.verification_steps.iter().any(|h| {
                h.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| id == orig_id)
                    .unwrap_or(false)
            });
            if !found {
                return Some(format!(
                    "stages[{}]: original step ID '{}' is missing from hardened output",
                    idx, orig_id
                ));
            }
        }

        // Non-verification phases within stages must be unchanged
        if orig_stage.setup_steps != hard_stage.setup_steps {
            return Some(format!("stages[{}].setup_steps were modified", idx));
        }
        if orig_stage.agentic_steps != hard_stage.agentic_steps {
            return Some(format!("stages[{}].agentic_steps were modified", idx));
        }
        if orig_stage.completion_steps != hard_stage.completion_steps {
            return Some(format!("stages[{}].completion_steps were modified", idx));
        }
    }

    // Check that all regression steps are preserved (top-level)
    let original_regression_ids: Vec<&str> = original
        .verification_steps
        .iter()
        .filter_map(|s| s.get("regression_issue_id").and_then(|v| v.as_str()))
        .collect();

    let hardened_regression_ids: Vec<&str> = hardened
        .verification_steps
        .iter()
        .filter_map(|s| s.get("regression_issue_id").and_then(|v| v.as_str()))
        .collect();

    for id in &original_regression_ids {
        if !hardened_regression_ids.contains(id) {
            return Some(format!(
                "Regression step for issue '{}' was removed by hardener",
                id
            ));
        }
    }

    // Check that all regression steps are preserved (per-stage)
    for (idx, (orig_stage, hard_stage)) in original
        .stages
        .iter()
        .zip(hardened.stages.iter())
        .enumerate()
    {
        let orig_stage_regression_ids: Vec<&str> = orig_stage
            .verification_steps
            .iter()
            .filter_map(|s| s.get("regression_issue_id").and_then(|v| v.as_str()))
            .collect();

        let hard_stage_regression_ids: Vec<&str> = hard_stage
            .verification_steps
            .iter()
            .filter_map(|s| s.get("regression_issue_id").and_then(|v| v.as_str()))
            .collect();

        for id in &orig_stage_regression_ids {
            if !hard_stage_regression_ids.contains(id) {
                return Some(format!(
                    "stages[{}]: regression step for issue '{}' was removed by hardener",
                    idx, id
                ));
            }
        }
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

    // Build a map of original step IDs to their types/modes/names for comparison
    // Include both top-level and stage verification steps
    let orig_map: std::collections::HashMap<&str, (&str, &str, &str)> = original
        .verification_steps
        .iter()
        .chain(
            original
                .stages
                .iter()
                .flat_map(|s| s.verification_steps.iter()),
        )
        .filter_map(|s| {
            let id = s.get("id").and_then(|v| v.as_str())?;
            let t = s.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let m = s.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            let n = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            Some((id, (t, m, n)))
        })
        .collect();

    // Check each hardened verification step against the original
    // Include both top-level and stage verification steps
    let all_hardened_verification: Vec<&Value> = hardened
        .verification_steps
        .iter()
        .chain(
            hardened
                .stages
                .iter()
                .flat_map(|s| s.verification_steps.iter()),
        )
        .collect();

    for hard_step in &all_hardened_verification {
        let hard_id = hard_step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let hard_type = hard_step
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let hard_mode = hard_step.get("mode").and_then(|v| v.as_str()).unwrap_or("");

        if let Some(&(orig_type, orig_mode, orig_name)) = orig_map.get(hard_id) {
            // This step existed in the original — check if type or mode changed
            let type_changed = orig_type != hard_type;
            let mode_changed = orig_type == "command"
                && hard_type == "command"
                && !orig_mode.is_empty()
                && !hard_mode.is_empty()
                && orig_mode != hard_mode;

            if type_changed || mode_changed {
                let orig_label = if orig_mode.is_empty() {
                    orig_type.to_string()
                } else {
                    format!("{}:{}", orig_type, orig_mode)
                };
                let new_label = if hard_mode.is_empty() {
                    hard_type.to_string()
                } else {
                    format!("{}:{}", hard_type, hard_mode)
                };
                converted_count += 1;
                conversions.push(HardeningConversion {
                    step_id: hard_id.to_string(),
                    original_name: orig_name.to_string(),
                    original_type: orig_label.clone(),
                    new_type: new_label.clone(),
                    explanation: format!("Converted from {} to {}", orig_label, new_label),
                });
            } else if orig_type == "prompt" {
                kept_as_prompt_count += 1;
            }
        } else {
            // New step (from splitting) — count as a conversion
            let hard_name = hard_step.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let hard_label = if hard_mode.is_empty() {
                hard_type.to_string()
            } else {
                format!("{}:{}", hard_type, hard_mode)
            };
            converted_count += 1;
            conversions.push(HardeningConversion {
                step_id: hard_id.to_string(),
                original_name: format!("(split from parent) {}", hard_name),
                original_type: "split".to_string(),
                new_type: hard_label.clone(),
                explanation: format!(
                    "New {} step from splitting a multi-concern step",
                    hard_label
                ),
            });
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
            enable_sweep: false,
            max_sweep_iterations: 5,
            generated_by_task_run_id: None,
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_sdk_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        let mut w = make_test_workflow(verification_steps);
        w.setup_steps = vec![json!({
            "id": "setup-sdk",
            "type": "command",
            "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/connect -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001\"}'"
        })];
        w
    }

    // === should_harden_verification tests ===

    #[test]
    fn test_should_harden_returns_false_for_all_deterministic() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/health | grep ok"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_true_for_prompt_verification() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "prompt", "phase": "verification", "content": "Check if the button exists"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_returns_false_for_empty_verification() {
        let workflow = make_test_workflow(vec![]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_playwright_with_sdk() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "playwright", "command": "npx playwright test ui-check"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_playwright_without_sdk() {
        let workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "playwright", "command": "npx playwright test"}),
        ]);
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_ui_test_with_sdk() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "test", "test_type": "custom_command", "name": "Verify page renders correctly", "command": "check-ui"}),
        ]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_detects_weak_sdk_shell_command() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:9876/ui-bridge/sdk/elements"
        })]);
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_ignores_strong_sdk_shell_command() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button-submit"
        })]);
        assert!(!should_harden_verification(&workflow));
    }

    // === is_ui_verification_test tests ===

    #[test]
    fn test_is_ui_test_detects_ui_keywords() {
        assert!(is_ui_verification_test(&json!({
            "name": "Verify page renders with element thumbnails",
            "command": "test"
        })));
        assert!(is_ui_verification_test(&json!({
            "name": "Check UI components",
            "command": "test"
        })));
    }

    #[test]
    fn test_is_ui_test_ignores_non_ui() {
        assert!(!is_ui_verification_test(&json!({
            "name": "Run unit tests",
            "command": "pytest tests/"
        })));
    }

    // === validate_hardened_output tests ===

    #[test]
    fn test_validate_rejects_step_count_decrease() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "prompt"}),
            json!({"id": "b", "type": "prompt"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("Step count decreased"));
    }

    #[test]
    fn test_validate_allows_step_count_increase_from_splitting() {
        let original = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
            json!({"id": "new-1", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/ai/search | grep nav"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    #[test]
    fn test_validate_rejects_missing_original_id() {
        let original = make_test_workflow(vec![json!({"id": "step-1", "type": "prompt"})]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-2", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("missing"));
    }

    #[test]
    fn test_validate_rejects_setup_modification() {
        let mut original = make_test_workflow(vec![json!({"id": "a", "type": "prompt"})]);
        original.setup_steps = vec![json!({"id": "setup-1", "type": "command", "mode": "shell"})];

        let mut hardened = make_test_workflow(vec![
            json!({"id": "a", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);
        hardened.setup_steps = vec![
            json!({"id": "setup-1", "type": "command", "mode": "shell", "command": "curl http://example.com"}),
        ];

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_some());
        assert!(error.unwrap().contains("setup_steps were modified"));
    }

    #[test]
    fn test_validate_accepts_valid_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "step-1", "type": "prompt", "content": "Check button"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "step-1", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
        ]);

        let error = validate_hardened_output(&original, &hardened);
        assert!(error.is_none());
    }

    // === build_summary tests ===

    #[test]
    fn test_summary_tracks_prompt_conversions() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "prompt"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check code", "type": "command", "mode": "check", "check_type": "lint"}),
            json!({"id": "s2", "name": "Check UI", "type": "prompt"}),
            json!({"id": "s3", "name": "Lint check", "type": "command", "mode": "check", "check_type": "lint"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert_eq!(summary.kept_as_prompt_count, 1);
        assert_eq!(summary.conversions.len(), 1);
        assert_eq!(summary.conversions[0].step_id, "s1");
        assert_eq!(summary.conversions[0].original_type, "prompt");
        assert!(summary.conversions[0].new_type.contains("command"));
    }

    #[test]
    fn test_summary_tracks_mode_conversion() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Playwright UI test", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Verify elements via SDK", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 1);
        assert!(summary.conversions[0].original_type.contains("test"));
        assert!(summary.conversions[0].new_type.contains("shell"));
    }

    #[test]
    fn test_summary_tracks_split_steps() {
        let original = make_test_workflow(vec![
            json!({"id": "s1", "name": "Big Playwright test", "type": "command", "mode": "test", "test_type": "playwright"}),
        ]);
        let hardened = make_test_workflow(vec![
            json!({"id": "s1", "name": "Check elements", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep button"}),
            json!({"id": "new-1", "name": "Check tabs", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep tab"}),
            json!({"id": "new-2", "name": "Check content", "type": "command", "mode": "shell", "command": "curl http://localhost:9876/ui-bridge/sdk/elements | grep content"}),
        ]);

        let summary = build_summary(&original, &hardened);
        assert_eq!(summary.converted_count, 3); // 1 mode change + 2 new splits
        assert_eq!(summary.conversions.len(), 3);
    }

    // === AppContext tests ===

    #[test]
    fn test_app_context_detects_web_app() {
        let mut workflow = make_test_workflow(vec![]);
        workflow.setup_steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl http://localhost:3001/api/test"
        })];
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.targets_web_app);
    }

    #[test]
    fn test_app_context_detects_sdk_connect() {
        let workflow = make_sdk_workflow(vec![]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.has_sdk_connect);
    }

    #[test]
    fn test_app_context_extracts_navigate_url() {
        let mut workflow = make_sdk_workflow(vec![]);
        workflow.setup_steps.push(json!({
            "id": "nav-1",
            "type": "command",
            "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001/management\"}'"
        }));
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert_eq!(
            ctx.setup_navigate_url.as_deref(),
            Some("http://localhost:3001/management")
        );
    }

    #[test]
    fn test_app_context_no_navigate_url_without_nav_step() {
        let workflow = make_sdk_workflow(vec![]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        assert!(ctx.setup_navigate_url.is_none());
    }

    #[test]
    fn test_hardener_prompt_includes_rule_4_when_sdk_connected() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "prompt", "content": "Check page"}),
        ]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt =
            build_hardener_prompt("{}", "test", &ctx, None, &SkillRegistry::new(), None, None, None);
        assert!(prompt.contains("Rule 4"));
        assert!(prompt.contains("page navigation"));
    }

    #[test]
    fn test_hardener_prompt_includes_sdk_response_guidance() {
        let workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "command", "command": "curl -s http://localhost:9876/ui-bridge/sdk/ai/search", "mode": "shell"}),
        ]);
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt =
            build_hardener_prompt("{}", "test", &ctx, None, &SkillRegistry::new(), None, None, None);
        assert!(prompt.contains("ai/search"));
        assert!(prompt.contains("total"));
        assert!(prompt.contains("grep"));
    }

    #[test]
    fn test_should_harden_detects_agentic_verification_gap() {
        // 3 agentic steps but only 2 deterministic verification steps → gap
        let mut workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "check", "check_type": "lint", "command": "npx eslint ."}),
        ]);
        workflow.agentic_steps = vec![
            json!({"id": "a1", "type": "prompt", "content": "Implement feature A"}),
            json!({"id": "a2", "type": "prompt", "content": "Implement feature B"}),
            json!({"id": "a3", "type": "prompt", "content": "Implement feature C"}),
        ];
        assert!(should_harden_verification(&workflow));
    }

    #[test]
    fn test_should_harden_no_gap_when_sufficient_coverage() {
        // 2 agentic steps with 3 deterministic verification steps → sufficient
        let mut workflow = make_test_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
            json!({"id": "s2", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/health | grep ok"}),
            json!({"id": "s3", "type": "command", "mode": "shell", "command": "curl http://localhost:3001/api/test | grep pass"}),
        ]);
        workflow.agentic_steps = vec![
            json!({"id": "a1", "type": "prompt", "content": "Fix feature A"}),
            json!({"id": "a2", "type": "prompt", "content": "Fix feature B"}),
        ];
        assert!(!should_harden_verification(&workflow));
    }

    #[test]
    fn test_hardener_prompt_includes_rule_5() {
        let mut workflow = make_sdk_workflow(vec![
            json!({"id": "s1", "type": "command", "mode": "check", "check_type": "typecheck", "command": "npx tsc --noEmit"}),
        ]);
        workflow.agentic_steps =
            vec![json!({"id": "a1", "type": "prompt", "content": "Implement thumbnails"})];
        let ctx = AppContext::from_workflow(&workflow, "test");
        let prompt =
            build_hardener_prompt("{}", "test", &ctx, None, &SkillRegistry::new(), None, None, None);
        assert!(prompt.contains("Rule 5"));
        assert!(prompt.contains("agentic step"));
        assert!(prompt.contains("verification coverage"));
    }

    // === fix_sdk_urls tests ===

    #[test]
    fn test_fix_sdk_urls_replaces_control_with_sdk() {
        let mut workflow = make_test_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -sf http://localhost:9876/ui-bridge/control/snapshot | grep elements"
        })]);
        // Add a setup step that references localhost:3001 (targets web)
        workflow.setup_steps = vec![json!({
            "id": "setup-sdk", "type": "command", "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/connect -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001\"}'"
        })];

        let fixed = fix_sdk_urls(&workflow);
        let cmd = fixed.verification_steps[0]
            .get("command")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            cmd.contains("ui-bridge/sdk/snapshot"),
            "Should replace /control/ with /sdk/: {}",
            cmd
        );
        assert!(
            !cmd.contains("ui-bridge/control/"),
            "Should not contain /control/: {}",
            cmd
        );
    }

    #[test]
    fn test_fix_sdk_urls_injects_connect_step_when_missing() {
        let mut workflow = make_test_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -sf http://localhost:9876/ui-bridge/control/snapshot"
        })]);
        // Setup references localhost:3001 but has no SDK connect
        workflow.setup_steps = vec![json!({
            "id": "nav-1", "type": "command", "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/control/page/navigate -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001/build\"}'"
        })];

        let fixed = fix_sdk_urls(&workflow);
        // Should have injected a connect step at position 0
        assert!(
            fixed.setup_steps.len() > workflow.setup_steps.len(),
            "Should inject connect step"
        );
        let first_cmd = fixed.setup_steps[0]
            .get("command")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            first_cmd.contains("ui-bridge/sdk/connect"),
            "First setup step should be SDK connect: {}",
            first_cmd
        );
        // The original nav step should also have /control/ replaced with /sdk/
        let nav_cmd = fixed.setup_steps[1]
            .get("command")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            nav_cmd.contains("ui-bridge/sdk/page/navigate"),
            "Nav step should use /sdk/: {}",
            nav_cmd
        );
    }

    #[test]
    fn test_fix_sdk_urls_noop_when_already_correct() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | grep elements"
        })]);
        let fixed = fix_sdk_urls(&workflow);
        assert_eq!(fixed.setup_steps.len(), workflow.setup_steps.len());
        assert_eq!(
            fixed.verification_steps.len(),
            workflow.verification_steps.len()
        );
    }

    #[test]
    fn test_fix_sdk_urls_injects_connect_when_sdk_urls_without_connect() {
        // AI generates /sdk/ URLs directly but forgets the connect step
        let mut workflow = make_test_workflow(vec![
            json!({
                "id": "s1", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | grep elements"
            }),
            json!({
                "id": "s2", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/elements | python -c \"import sys,json; d=json.load(sys.stdin); assert d.get('total',0)>0\""
            }),
        ]);
        // Setup navigates to localhost:3001 but has no SDK connect
        workflow.setup_steps = vec![json!({
            "id": "nav-1", "type": "command", "mode": "shell",
            "command": "curl -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -H 'Content-Type: application/json' -d '{\"url\": \"http://localhost:3001/build/page-sweep\"}'"
        })];

        let fixed = fix_sdk_urls(&workflow);
        // Should have injected a connect step at position 0
        assert!(
            fixed.setup_steps.len() > workflow.setup_steps.len(),
            "Should inject connect step"
        );
        let first_cmd = fixed.setup_steps[0]
            .get("command")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            first_cmd.contains("ui-bridge/sdk/connect"),
            "First setup step should be SDK connect: {}",
            first_cmd
        );
        assert!(
            first_cmd.contains("localhost:3001"),
            "Connect step should target localhost:3001: {}",
            first_cmd
        );
    }

    #[test]
    fn test_fix_sdk_urls_fixes_check_url_field() {
        let workflow = make_sdk_workflow(vec![json!({
            "id": "s1", "type": "command", "mode": "check",
            "check_type": "http_status",
            "check_url": "http://localhost:9876/ui-bridge/control/elements",
            "expected_status": 200
        })]);

        let fixed = fix_sdk_urls(&workflow);
        let check_url = fixed.verification_steps[0]
            .get("check_url")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(
            check_url.contains("ui-bridge/sdk/elements"),
            "check_url should be fixed: {}",
            check_url
        );
    }

    // === inject_retries_after_navigation tests ===

    #[test]
    fn test_inject_retries_adds_retries_after_nav() {
        let mut steps = vec![
            json!({
                "id": "nav", "type": "command", "mode": "shell",
                "command": "curl -sf -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -d '{\"url\":\"http://localhost:3001/build\"}'",
                "name": "Navigate"
            }),
            json!({
                "id": "s1", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | grep elements",
                "name": "Check snapshot"
            }),
            json!({
                "id": "s2", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/elements | grep button",
                "name": "Check elements"
            }),
        ];

        let count = inject_retries_after_navigation(&mut steps);
        assert_eq!(count, 2, "Should add retries to 2 SDK steps after nav");
        assert_eq!(steps[1].get("retry_count").unwrap().as_u64().unwrap(), 5);
        assert_eq!(
            steps[1].get("retry_delay_ms").unwrap().as_u64().unwrap(),
            3000
        );
        assert_eq!(steps[2].get("retry_count").unwrap().as_u64().unwrap(), 5);
        // Nav step itself should not get retries
        assert!(steps[0].get("retry_count").is_none());
    }

    #[test]
    fn test_inject_retries_stops_at_non_sdk_step() {
        let mut steps = vec![
            json!({
                "id": "nav", "type": "command", "mode": "shell",
                "command": "curl -sf -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -d '{}'",
                "name": "Navigate"
            }),
            json!({
                "id": "s1", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot",
                "name": "Check snapshot"
            }),
            json!({
                "id": "s2", "type": "command", "mode": "check",
                "check_type": "typecheck",
                "command": "npx tsc --noEmit",
                "name": "Typecheck"
            }),
            json!({
                "id": "s3", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/elements",
                "name": "Check elements after typecheck"
            }),
        ];

        let count = inject_retries_after_navigation(&mut steps);
        assert_eq!(
            count, 1,
            "Should only add retries to the 1 SDK step before typecheck"
        );
        assert!(steps[1].get("retry_count").is_some());
        assert!(steps[2].get("retry_count").is_none());
        assert!(
            steps[3].get("retry_count").is_none(),
            "SDK step after non-SDK should not get retries"
        );
    }

    #[test]
    fn test_inject_retries_preserves_existing_retries() {
        let mut steps = vec![
            json!({
                "id": "nav", "type": "command", "mode": "shell",
                "command": "curl -sf -X POST http://localhost:9876/ui-bridge/sdk/page/navigate -d '{}'",
                "name": "Navigate"
            }),
            json!({
                "id": "s1", "type": "command", "mode": "shell",
                "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot",
                "name": "Check",
                "retry_count": 10,
                "retry_delay_ms": 5000
            }),
        ];

        let count = inject_retries_after_navigation(&mut steps);
        assert_eq!(
            count, 0,
            "Should not modify steps that already have retry_count"
        );
        assert_eq!(steps[1].get("retry_count").unwrap().as_u64().unwrap(), 10);
    }

    // ====================================================================
    // Command sanitizer tests
    // ====================================================================

    #[test]
    fn test_replace_jq_with_python_elements() {
        // SDK /elements returns {data: [...]} not {elements: [], total: N}
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/elements | jq -e ".total > 0""#;
        let result = replace_jq_with_python(cmd);
        assert!(result.is_some(), "Should replace jq with python");
        let py_cmd = result.unwrap();
        assert!(py_cmd.contains("python -c"), "Should use python -c");
        assert!(!py_cmd.contains("jq"), "Should not contain jq");
        // For SDK /elements, total is mapped to len(data) since the API has no total field
        assert!(
            py_cmd.contains("data"),
            "Should use 'data' key for SDK elements: {}",
            py_cmd
        );
        assert!(
            py_cmd.contains("len(items)>0"),
            "Should check len(data) > 0: {}",
            py_cmd
        );
    }

    #[test]
    fn test_replace_jq_with_python_element_length() {
        // /snapshot returns {data: {elements: [...]}} — elements key exists under data
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | jq -e '.elements | length > 2'"#;
        let result = replace_jq_with_python(cmd);
        assert!(result.is_some(), "Should replace jq with python");
        let py_cmd = result.unwrap();
        assert!(
            py_cmd.contains("len(elems)>2"),
            "Should check elements length > 2: {}",
            py_cmd
        );
    }

    #[test]
    fn test_sanitize_commands_replaces_jq() {
        let mut steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -sf http://localhost:9876/ui-bridge/sdk/elements | jq -e \".total > 0\"",
            "name": "Check elements"
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert!(count >= 1, "Should sanitize at least 1 step");
        let cmd = steps[0].get("command").unwrap().as_str().unwrap();
        assert!(cmd.contains("python"), "Should use python now");
        assert!(!cmd.contains("jq"), "Should not contain jq");
    }

    #[test]
    fn test_sanitize_commands_fixes_nested_retry() {
        let mut steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | grep elements",
            "name": "Check",
            "retry": {"count": 5, "delay_ms": 3000}
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert!(count >= 1, "Should fix retry format");
        assert_eq!(steps[0].get("retry_count").unwrap().as_u64().unwrap(), 5);
        assert_eq!(
            steps[0].get("retry_delay_ms").unwrap().as_u64().unwrap(),
            3000
        );
        assert!(
            steps[0].get("retry").is_none(),
            "Nested retry should be removed"
        );
    }

    #[test]
    fn test_sanitize_commands_leaves_non_jq_alone() {
        let mut steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "curl -s http://localhost:9876/ui-bridge/sdk/snapshot | grep 'elements'",
            "name": "Simple grep"
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert_eq!(count, 0, "Should not modify non-jq commands");
    }

    #[test]
    fn test_sanitize_python_fstring_with_clean() {
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | python -c "import sys,json; d=json.load(sys.stdin); assert d.get('elements') and len(d['elements'])>2, f'Expected >2 elements, got {len(d.get(\"elements\",[]))}'"#;
        let result = replace_python_fstring_with_clean(cmd);
        assert!(result.is_some(), "Should replace fstring python");
        let clean = result.unwrap();
        assert!(!clean.contains("f'"), "Should not contain f-strings");
        assert!(clean.contains("len(elems)>2"), "Should check elements > 2");
    }

    #[test]
    fn test_extract_number_threshold() {
        assert_eq!(extract_number_threshold("len(x)>2"), Some(2));
        assert_eq!(extract_number_threshold("total > 0"), Some(0));
        assert_eq!(extract_number_threshold("len(d['elements'])>10"), Some(10));
        assert_eq!(extract_number_threshold("no threshold here"), None);
    }

    // === quote_curl_urls_with_ampersand tests ===

    #[test]
    fn test_quote_url_with_ampersand() {
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true&contentTypes=heading | grep -i "sweep""#;
        let result = quote_curl_urls_with_ampersand(cmd);
        assert!(result.is_some(), "Should quote URL with &");
        let fixed = result.unwrap();
        assert!(
            fixed.contains(r#""http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true&contentTypes=heading""#),
            "URL should be wrapped in double quotes: {}",
            fixed
        );
        assert!(
            fixed.contains(r#"| grep -i "sweep""#),
            "grep part should be preserved: {}",
            fixed
        );
    }

    #[test]
    fn test_quote_url_no_ampersand_noop() {
        let cmd =
            "curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true | grep button";
        let result = quote_curl_urls_with_ampersand(cmd);
        assert!(result.is_none(), "Should not modify URL without &");
    }

    #[test]
    fn test_quote_url_already_quoted_noop() {
        let cmd = r#"curl -sf "http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true&contentTypes=heading" | grep button"#;
        let result = quote_curl_urls_with_ampersand(cmd);
        assert!(result.is_none(), "Should not modify already-quoted URL");
    }

    #[test]
    fn test_quote_url_no_curl_noop() {
        let cmd = "python -c 'import sys; print(sys.argv)' & echo done";
        let result = quote_curl_urls_with_ampersand(cmd);
        assert!(result.is_none(), "Should not modify non-curl commands");
    }

    #[test]
    fn test_sanitize_commands_replaces_bash_negation() {
        let mut steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": "! grep -qE 'RoutingStatusSection|RetryStatusSection' file.tsx",
            "name": "Old imports removed"
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert!(count >= 1, "Should sanitize bash negation prefix");
        let cmd = steps[0].get("command").unwrap().as_str().unwrap();
        assert!(
            cmd.contains("&& exit 1 || exit 0"),
            "Should convert ! prefix to explicit exit code handling: {}",
            cmd
        );
        assert!(
            !cmd.starts_with("! "),
            "Should not start with bash negation: {}",
            cmd
        );
    }

    #[test]
    fn test_sanitize_commands_quotes_curl_urls() {
        let mut steps = vec![json!({
            "id": "s1", "type": "command", "mode": "shell",
            "command": r#"curl -sf http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true&contentTypes=heading | grep -i "sweep""#,
            "name": "Check heading"
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert!(count >= 1, "Should sanitize URL with &");
        let cmd = steps[0].get("command").unwrap().as_str().unwrap();
        assert!(
            cmd.contains(r#""http://localhost:9876/ui-bridge/sdk/elements?contentOnly=true&contentTypes=heading""#),
            "URL should be quoted: {}",
            cmd
        );
    }

    // === fix_curl_sf_in_piped_commands tests ===

    #[test]
    fn test_curl_sf_removed_in_piped_command() {
        let cmd = r#"curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | python -c "import sys,json; d=json.load(sys.stdin)""#;
        let fixed = fix_curl_sf_in_piped_commands(cmd);
        assert!(
            !fixed.contains("-sf"),
            "Should remove -f from -sf: {}",
            fixed
        );
        assert!(fixed.contains("-s "), "Should keep -s: {}", fixed);
    }

    #[test]
    fn test_curl_fs_removed_in_piped_command() {
        let cmd = "curl -fs http://example.com/api | grep ok";
        let fixed = fix_curl_sf_in_piped_commands(cmd);
        assert!(!fixed.contains('f'), "Should remove f from -fs: {}", fixed);
        assert!(fixed.contains("-s"), "Should keep -s: {}", fixed);
    }

    #[test]
    fn test_curl_sfL_becomes_sL() {
        let cmd = "curl -sfL http://example.com | python -c 'print(1)'";
        let fixed = fix_curl_sf_in_piped_commands(cmd);
        assert_eq!(fixed.contains("-sL"), true, "Should become -sL: {}", fixed);
        assert!(!fixed.contains('f'), "Should not contain f: {}", fixed);
    }

    #[test]
    fn test_curl_s_not_changed_when_no_pipe() {
        let cmd = "curl -sf http://example.com";
        let fixed = fix_curl_sf_in_piped_commands(cmd);
        assert_eq!(fixed, cmd, "Should not change command without pipe");
    }

    #[test]
    fn test_curl_s_not_changed_when_already_correct() {
        let cmd = "curl -s http://example.com | grep ok";
        let fixed = fix_curl_sf_in_piped_commands(cmd);
        assert_eq!(fixed, cmd, "Should not change command without -f");
    }

    #[test]
    fn test_sanitize_fixes_curl_sf_in_piped_steps() {
        let mut steps = vec![json!({
            "command": r#"curl -sf http://localhost:9876/ui-bridge/sdk/snapshot | python -c "import sys,json; d=json.load(sys.stdin)""#,
            "name": "Check snapshot"
        })];
        let count = sanitize_commands_in_steps(&mut steps);
        assert!(count >= 1, "Should sanitize curl -sf in piped command");
        let cmd = steps[0].get("command").unwrap().as_str().unwrap();
        assert!(!cmd.contains("-sf"), "Should not contain -sf: {}", cmd);
        assert!(cmd.contains("-s "), "Should contain -s: {}", cmd);
    }
}

// ==========================================================================
// Retry Standardization (Feature 6)
// ==========================================================================

/// Get default retry policy for a step based on its type and content.
///
/// Returns `(count, delay_ms)` or `None` if no retry is appropriate.
pub fn retry_defaults(step: &serde_json::Value) -> Option<(u32, u64)> {
    let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let cmd = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let check_type = step
        .get("check_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match step_type {
        "command" => {
            if check_type == "http_status" || cmd.contains("curl") {
                // HTTP health checks: 3 retries, 2s delay
                Some((3, 2000))
            } else if cmd.contains("sqlite3") || cmd.contains(".db") {
                // SQLite queries: no retries (deterministic)
                None
            } else if check_type == "ai_review" {
                // AI review: 1 retry, 5s delay
                Some((1, 5000))
            } else {
                None
            }
        }
        "ui_bridge" => {
            if action == "assert" || action == "snapshot_assert" {
                // UI Bridge assertions: 2 retries, 1s delay
                Some((2, 1000))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Apply retry policies to all steps in a workflow.
///
/// Steps that already have a retry config are left unchanged.
/// Stage-level retry_policy overrides per-step defaults for steps within that stage.
pub fn apply_retry_policies(workflow: &mut crate::unified_workflows::UnifiedWorkflow) {
    // Apply to top-level verification steps
    apply_retry_to_steps(&mut workflow.verification_steps, None);

    // Apply to stage verification steps
    for stage in &mut workflow.stages {
        let stage_policy = stage.retry_policy.as_ref().map(|p| (p.count, p.delay_ms));
        apply_retry_to_steps(&mut stage.verification_steps, stage_policy);
    }
}

fn apply_retry_to_steps(steps: &mut [serde_json::Value], stage_override: Option<(u32, u64)>) {
    for step in steps.iter_mut() {
        // Skip steps that already have retry configured (flat format)
        if step.get("retry_count").is_some() {
            continue;
        }

        // Determine retry config: stage override > step defaults
        let retry_config = if let Some((count, delay_ms)) = stage_override {
            Some((count, delay_ms))
        } else {
            retry_defaults(step)
        };

        if let Some((count, delay_ms)) = retry_config {
            if count > 0 {
                if let Some(obj) = step.as_object_mut() {
                    obj.insert("retry_count".to_string(), serde_json::json!(count));
                    obj.insert("retry_delay_ms".to_string(), serde_json::json!(delay_ms));
                }
            }
        }
    }
}

// ==========================================================================
// Required-Flag Enforcement (Feature 7)
// ==========================================================================

/// Enforce required=true on verification steps linked to Critical criteria.
///
/// Called after the hardener agent returns, before the revision phase.
pub fn enforce_required_flag_discipline(
    workflow: &mut crate::unified_workflows::UnifiedWorkflow,
    criteria: Option<&crate::workflow_generation::specification::AcceptanceCriteria>,
) {
    use crate::workflow_generation::specification::CriterionPriority;

    let criteria = match criteria {
        Some(c) => c,
        None => return,
    };

    // Build set of critical criterion IDs
    let critical_ids: std::collections::HashSet<&str> = criteria
        .criteria
        .iter()
        .filter(|c| c.priority == CriterionPriority::Critical)
        .map(|c| c.id.as_str())
        .collect();

    if critical_ids.is_empty() {
        return;
    }

    // Enforce on top-level verification steps
    enforce_required_on_steps(&mut workflow.verification_steps, &critical_ids);

    // Enforce on stage verification steps
    for stage in &mut workflow.stages {
        enforce_required_on_steps(&mut stage.verification_steps, &critical_ids);
    }
}

fn enforce_required_on_steps(
    steps: &mut [serde_json::Value],
    critical_ids: &std::collections::HashSet<&str>,
) {
    for step in steps.iter_mut() {
        let mut is_critical = false;

        if let Some(cid) = step.get("criterion_id").and_then(|v| v.as_str()) {
            if critical_ids.contains(cid) {
                is_critical = true;
            }
        }

        if let Some(cids) = step.get("criterion_ids").and_then(|v| v.as_array()) {
            for cid in cids {
                if let Some(cid_str) = cid.as_str() {
                    if critical_ids.contains(cid_str) {
                        is_critical = true;
                        break;
                    }
                }
            }
        }

        if is_critical {
            if let Some(obj) = step.as_object_mut() {
                obj.insert("required".to_string(), serde_json::Value::Bool(true));
            }
        }
    }
}

/// Validate a prompt for basic robustness red flags (static analysis, no LLM).
///
/// Checks for patterns that indicate the prompt may be vulnerable to injection,
/// overly permissive, or missing safety boundaries.
///
/// Returns a list of warnings (empty = no issues found).
pub fn validate_prompt_robustness(prompt: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    // Check for missing role/boundary instructions
    let lower = prompt.to_lowercase();
    if !lower.contains("you are") && !lower.contains("your role") && !lower.contains("as a") {
        warnings.push(
            "Prompt has no explicit role definition — vulnerable to role injection".to_string(),
        );
    }

    // Check for overly permissive instructions
    if lower.contains("do anything")
        || lower.contains("no restrictions")
        || lower.contains("ignore safety")
    {
        warnings.push("Prompt contains overly permissive language".to_string());
    }

    // Check for missing output format constraints
    if !lower.contains("json")
        && !lower.contains("format")
        && !lower.contains("respond with")
        && !lower.contains("output")
    {
        warnings.push(
            "Prompt has no output format constraints — may produce unparseable results".to_string(),
        );
    }

    // Check prompt length (too short = likely underspecified)
    if prompt.len() < 50 {
        warnings.push(format!(
            "Prompt is very short ({} chars) — may be underspecified",
            prompt.len()
        ));
    }

    // Check for common injection vectors not being addressed
    if !lower.contains("ignore")
        && !lower.contains("override")
        && !lower.contains("previous instructions")
    {
        // The prompt doesn't mention these terms, which is fine — but if the prompt
        // doesn't have any boundary-setting language at all, flag it
        if !lower.contains("must")
            && !lower.contains("always")
            && !lower.contains("never")
            && !lower.contains("do not")
        {
            warnings.push(
                "Prompt lacks boundary-setting language (must/always/never/do not)".to_string(),
            );
        }
    }

    warnings
}

#[cfg(test)]
mod robustness_tests {
    use super::*;

    #[test]
    fn test_validate_prompt_robustness_good_prompt() {
        let prompt = "You are a code verification agent. Your role is to analyze code changes. \
                      Always respond with JSON format. You must never execute arbitrary commands. \
                      Do not modify files outside the working directory.";
        let warnings = validate_prompt_robustness(prompt);
        assert!(
            warnings.is_empty(),
            "Good prompt should have no warnings: {:?}",
            warnings
        );
    }

    #[test]
    fn test_validate_prompt_robustness_bad_prompt() {
        let prompt = "fix it";
        let warnings = validate_prompt_robustness(prompt);
        assert!(!warnings.is_empty(), "Bad prompt should have warnings");
        assert!(warnings.iter().any(|w| w.contains("very short")));
    }

    #[test]
    fn test_validate_prompt_robustness_no_role() {
        let prompt = "Please analyze the following code and return results in JSON format. You must check for errors.";
        let warnings = validate_prompt_robustness(prompt);
        // Has format constraints and boundary language, but no role definition
        assert!(
            warnings.iter().any(|w| w.contains("role definition")),
            "Should warn about missing role: {:?}",
            warnings
        );
    }
}
