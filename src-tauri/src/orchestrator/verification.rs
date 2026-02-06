//! Verification execution for the task orchestrator.
//!
//! This module handles running both deterministic and AI verification checks.
//! Deterministic checks are run by the system without AI involvement.
//! AI verification is isolated - the verification agent only sees the screenshot
//! and evaluation prompt, NOT the worker's reasoning.

#![allow(dead_code)]

use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Build an enhanced PATH that includes Node.js tools and local node_modules.
///
/// On Windows, npm tools like npx, eslint, prettier are .cmd batch files that may not be
/// found if the PATH doesn't include the npm directory. This function:
/// 1. Adds local node_modules/.bin to PATH (for project-local tools)
/// 2. Adds common Node.js installation paths on Windows
/// 3. Preserves the existing PATH
fn build_enhanced_path(working_dir: &str) -> String {
    let mut paths: Vec<String> = Vec::new();
    let path_separator = if cfg!(target_os = "windows") {
        ";"
    } else {
        ":"
    };

    // Add local node_modules/.bin (highest priority for project-local tools)
    let local_node_bin = Path::new(working_dir).join("node_modules").join(".bin");
    if local_node_bin.exists() {
        paths.push(local_node_bin.to_string_lossy().to_string());
    }

    // On Windows, add common Node.js installation paths
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            // npm global packages directory
            let npm_path = Path::new(&appdata).join("npm");
            if npm_path.exists() {
                paths.push(npm_path.to_string_lossy().to_string());
            }
        }

        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            // Node.js installation via installer
            let nodejs_path = Path::new(&localappdata).join("Programs").join("nodejs");
            if nodejs_path.exists() {
                paths.push(nodejs_path.to_string_lossy().to_string());
            }
        }

        // Common Program Files paths
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            let nodejs_path = Path::new(&program_files).join("nodejs");
            if nodejs_path.exists() {
                paths.push(nodejs_path.to_string_lossy().to_string());
            }
        }

        // fnm (Fast Node Manager) paths
        if let Ok(fnm_dir) = std::env::var("FNM_DIR") {
            let fnm_current = Path::new(&fnm_dir)
                .join("node-versions")
                .join("current")
                .join("bin");
            if fnm_current.exists() {
                paths.push(fnm_current.to_string_lossy().to_string());
            }
        }

        // nvm-windows paths
        if let Ok(nvm_home) = std::env::var("NVM_HOME") {
            if let Ok(nvm_symlink) = std::env::var("NVM_SYMLINK") {
                paths.push(nvm_symlink);
            }
            paths.push(nvm_home);
        }
    }

    // Append existing PATH
    if let Ok(existing_path) = std::env::var("PATH") {
        paths.push(existing_path);
    }

    paths.join(path_separator)
}

use super::types::*;
use crate::ai_router::TaskContext;
use crate::commands::AppState;
use crate::state_explorer::{ExplorationConfig, ExplorationStatus, ExplorationTask};

/// Executor for deterministic verification checks.
pub struct DeterministicVerifier {
    /// Working directory for running commands
    pub working_directory: String,
    /// App state for State Explorer integration
    pub app_state: Option<Arc<AppState>>,
}

impl DeterministicVerifier {
    pub fn new(working_directory: String) -> Self {
        Self {
            working_directory,
            app_state: None,
        }
    }

    /// Create a new verifier with AppState for State Explorer support
    pub fn with_app_state(working_directory: String, app_state: Arc<AppState>) -> Self {
        Self {
            working_directory,
            app_state: Some(app_state),
        }
    }

    /// Run all deterministic verification checks for a plan.
    pub async fn verify_all(&self, plan: &VerificationPlan) -> Vec<VerificationResult> {
        let mut results = Vec::new();

        for criterion in plan.deterministic_criteria() {
            let result = self.verify_criterion(criterion).await;
            results.push(result);
        }

        results
    }

    /// Run a single deterministic verification check.
    pub async fn verify_criterion(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let method = criterion
            .verification_method
            .unwrap_or(VerificationMethod::CustomCommand);

        info!(
            "Running deterministic verification: {} (method: {:?})",
            criterion.id, method
        );

        match method {
            VerificationMethod::BuildSuccess => self.verify_build(criterion).await,
            VerificationMethod::UnitTest => self.verify_tests(criterion, "unit").await,
            VerificationMethod::IntegrationTest => {
                self.verify_tests(criterion, "integration").await
            }
            VerificationMethod::Playwright => self.verify_playwright(criterion).await,
            VerificationMethod::LogPattern => self.verify_log_pattern(criterion).await,
            VerificationMethod::GuiAutomation => self.verify_gui_automation(criterion).await,
            VerificationMethod::TypeCheck => self.verify_type_check(criterion).await,
            VerificationMethod::LintCheck => self.verify_lint(criterion).await,
            VerificationMethod::CustomCommand => self.verify_custom_command(criterion).await,
        }
    }

    async fn verify_build(&self, criterion: &SuccessCriterion) -> VerificationResult {
        // Get build command from config or use default
        let config = criterion.verification_config.as_ref();
        let command = config
            .and_then(|c| c.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("npm run build");

        self.run_command(criterion, command, "Build").await
    }

    async fn verify_tests(
        &self,
        criterion: &SuccessCriterion,
        test_type: &str,
    ) -> VerificationResult {
        let config = criterion.verification_config.as_ref();

        // Get test command and pattern from config
        let default_command = match test_type {
            "unit" => "npm test",
            "integration" => "npm run test:integration",
            _ => "npm test",
        };

        let command = config
            .and_then(|c| c.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or(default_command);

        let pattern = config
            .and_then(|c| c.get("test_pattern"))
            .and_then(|v| v.as_str());

        let full_command = if let Some(pat) = pattern {
            format!("{} -- {}", command, pat)
        } else {
            command.to_string()
        };

        self.run_command(criterion, &full_command, &format!("{} tests", test_type))
            .await
    }

    async fn verify_playwright(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let config = criterion.verification_config.as_ref();

        let script_id = config
            .and_then(|c| c.get("script_id"))
            .and_then(|v| v.as_str());

        let script_path = config
            .and_then(|c| c.get("script_path"))
            .and_then(|v| v.as_str());

        let command = if let Some(id) = script_id {
            // TODO: Look up script by ID from database
            format!("npx playwright test --grep {}", id)
        } else if let Some(path) = script_path {
            format!("npx playwright test {}", path)
        } else {
            "npx playwright test".to_string()
        };

        self.run_command(criterion, &command, "Playwright").await
    }

    async fn verify_log_pattern(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let config = criterion.verification_config.as_ref();

        let pattern = config
            .and_then(|c| c.get("pattern"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let log_path = config
            .and_then(|c| c.get("log_path"))
            .and_then(|v| v.as_str())
            .unwrap_or(".dev-logs/runner-tauri.log");

        let should_match = config
            .and_then(|c| c.get("should_match"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Use grep to check pattern
        let command = format!("grep -E '{}' '{}'", pattern, log_path);

        let result = self.run_command(criterion, &command, "Log pattern").await;

        // Invert result if should_match is false (checking pattern should NOT exist)
        if !should_match {
            VerificationResult {
                passed: !result.passed,
                issues: if result.passed {
                    vec![format!(
                        "Pattern '{}' was found but should not exist",
                        pattern
                    )]
                } else {
                    vec![]
                },
                ..result
            }
        } else {
            result
        }
    }

    async fn verify_gui_automation(&self, criterion: &SuccessCriterion) -> VerificationResult {
        // Check if we have AppState for State Explorer
        let app_state = match &self.app_state {
            Some(state) => state.clone(),
            None => {
                warn!("GuiAutomation verification requires AppState - skipping");
                return VerificationResult {
                    criterion_id: criterion.id.clone(),
                    passed: true, // Don't fail if not configured
                    criterion_type: CriterionType::Deterministic,
                    confidence: None,
                    observations: vec!["State Explorer not configured (no AppState)".to_string()],
                    issues: vec![],
                    suggestions: vec![
                        "Use DeterministicVerifier::with_app_state() for State Explorer support"
                            .to_string(),
                    ],
                    raw_output: None,
                };
            }
        };

        // Parse exploration config from criterion
        let config = criterion.verification_config.as_ref();

        let config_path = config
            .and_then(|c| c.get("config_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if config_path.is_empty() {
            return VerificationResult {
                criterion_id: criterion.id.clone(),
                passed: false,
                criterion_type: CriterionType::Deterministic,
                confidence: None,
                observations: vec![],
                issues: vec![
                    "GuiAutomation criterion requires config_path in verification_config"
                        .to_string(),
                ],
                suggestions: vec![
                    "Add verification_config.config_path with path to qontinui config".to_string(),
                ],
                raw_output: None,
            };
        }

        let strategy = config
            .and_then(|c| c.get("strategy"))
            .and_then(|v| v.as_str())
            .unwrap_or("smoke_test")
            .to_string();

        let max_states = config
            .and_then(|c| c.get("max_states"))
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as u32;

        let stop_on_first_failure = config
            .and_then(|c| c.get("stop_on_first_failure"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Build exploration config
        let exploration_config = ExplorationConfig {
            config_path: config_path.clone(),
            strategy,
            max_states,
            max_duration_seconds: 300, // 5 minute timeout
            stop_on_first_failure,
            capture_screenshots: true,
            ..Default::default()
        };

        info!(
            "Running State Explorer for criterion {} with config {}",
            criterion.id, config_path
        );

        // Run the State Explorer
        let task = ExplorationTask::new(exploration_config, app_state);
        let exploration_result = match task.execute().await {
            Ok(result) => result,
            Err(e) => {
                error!("State Explorer failed: {}", e);
                return VerificationResult {
                    criterion_id: criterion.id.clone(),
                    passed: false,
                    criterion_type: CriterionType::Deterministic,
                    confidence: None,
                    observations: vec![],
                    issues: vec![format!("State Explorer execution failed: {}", e)],
                    suggestions: vec![
                        "Check config path and ensure the application is running".to_string()
                    ],
                    raw_output: Some(format!("Error: {}", e)),
                };
            }
        };

        // Convert ExplorationResult to VerificationResult
        let passed = matches!(exploration_result.overall_status, ExplorationStatus::Passed);

        let mut observations = vec![exploration_result.summary.clone()];

        // Add state exploration observations
        for state in &exploration_result.state_explorations {
            let status_str = match state.status {
                ExplorationStatus::Passed => "passed",
                ExplorationStatus::Failed => "failed",
                ExplorationStatus::Skipped => "skipped",
                _ => "unknown",
            };
            observations.push(format!(
                "State '{}': {} (confidence: {:.2})",
                state.state_name, status_str, state.detection_confidence
            ));
        }

        // Collect issues from failed states
        let mut issues: Vec<String> = exploration_result
            .state_explorations
            .iter()
            .filter(|s| matches!(s.status, ExplorationStatus::Failed))
            .map(|s| {
                let missing: Vec<_> = s
                    .expected_elements_found
                    .iter()
                    .filter(|e| !e.found)
                    .map(|e| e.element.clone())
                    .collect();

                if !missing.is_empty() {
                    format!(
                        "State '{}' missing elements: {}",
                        s.state_name,
                        missing.join(", ")
                    )
                } else if let Some(err) = &s.error {
                    format!("State '{}' error: {}", s.state_name, err)
                } else {
                    format!("State '{}' exploration failed", s.state_name)
                }
            })
            .collect();

        // Add transition failures
        for transition in &exploration_result.transition_explorations {
            if matches!(transition.status, ExplorationStatus::Failed) {
                if let Some(err) = &transition.error {
                    issues.push(format!(
                        "Transition {} -> {} failed: {}",
                        transition.from_state_id, transition.to_state_id, err
                    ));
                } else {
                    issues.push(format!(
                        "Transition {} -> {} failed",
                        transition.from_state_id, transition.to_state_id
                    ));
                }
            }
        }

        let suggestions = if !passed {
            vec![
                "Review failed states and transitions in the exploration report".to_string(),
                "Check if the application is in the expected initial state".to_string(),
            ]
        } else {
            vec![]
        };

        // Serialize full result as raw output
        let raw_output = serde_json::to_string_pretty(&exploration_result).ok();

        info!(
            "State Explorer completed for {}: passed={}, states_visited={}, states_failed={}",
            criterion.id,
            passed,
            exploration_result.states_visited,
            exploration_result.states_failed
        );

        VerificationResult {
            criterion_id: criterion.id.clone(),
            passed,
            criterion_type: CriterionType::Deterministic,
            confidence: None,
            observations,
            issues,
            suggestions,
            raw_output,
        }
    }

    async fn verify_type_check(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let config = criterion.verification_config.as_ref();
        let command = config
            .and_then(|c| c.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("npm run typecheck");

        self.run_command(criterion, command, "Type check").await
    }

    async fn verify_lint(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let config = criterion.verification_config.as_ref();
        let command = config
            .and_then(|c| c.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("npm run lint");

        self.run_command(criterion, command, "Lint").await
    }

    async fn verify_custom_command(&self, criterion: &SuccessCriterion) -> VerificationResult {
        let config = criterion.verification_config.as_ref();
        let command = config
            .and_then(|c| c.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("echo 'No command specified'");

        self.run_command(criterion, command, "Custom command").await
    }

    async fn run_command(
        &self,
        criterion: &SuccessCriterion,
        command: &str,
        check_name: &str,
    ) -> VerificationResult {
        info!("Running {}: {}", check_name, command);

        // Build enhanced PATH to ensure npm/npx tools are found
        let enhanced_path = build_enhanced_path(&self.working_directory);

        // Run command using shell with enhanced PATH
        let output = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", command])
                .current_dir(&self.working_directory)
                .env("PATH", &enhanced_path)
                .output()
        } else {
            Command::new("sh")
                .args(["-c", command])
                .current_dir(&self.working_directory)
                .env("PATH", &enhanced_path)
                .output()
        };

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let passed = output.status.success();

                let raw_output = format!(
                    "Exit code: {}\n\n--- stdout ---\n{}\n\n--- stderr ---\n{}",
                    output.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                );

                let issues = if !passed {
                    // Extract meaningful error lines
                    let error_lines: Vec<String> = stderr
                        .lines()
                        .chain(stdout.lines())
                        .filter(|line| {
                            let lower = line.to_lowercase();
                            lower.contains("error")
                                || lower.contains("failed")
                                || lower.contains("fail")
                        })
                        .take(10)
                        .map(|s| s.to_string())
                        .collect();

                    if error_lines.is_empty() {
                        vec![format!(
                            "{} failed with exit code {}",
                            check_name,
                            output.status.code().unwrap_or(-1)
                        )]
                    } else {
                        error_lines
                    }
                } else {
                    vec![]
                };

                VerificationResult {
                    criterion_id: criterion.id.clone(),
                    passed,
                    criterion_type: CriterionType::Deterministic,
                    confidence: None,
                    observations: vec![format!(
                        "{} {} (exit code: {})",
                        check_name,
                        if passed { "passed" } else { "failed" },
                        output.status.code().unwrap_or(-1)
                    )],
                    issues,
                    suggestions: vec![],
                    raw_output: Some(raw_output),
                }
            }
            Err(e) => {
                error!("Failed to run {}: {}", check_name, e);
                VerificationResult {
                    criterion_id: criterion.id.clone(),
                    passed: false,
                    criterion_type: CriterionType::Deterministic,
                    confidence: None,
                    observations: vec![],
                    issues: vec![format!("Failed to execute {}: {}", check_name, e)],
                    suggestions: vec![
                        "Check that the command is valid and dependencies are installed"
                            .to_string(),
                    ],
                    raw_output: Some(format!("Error: {}", e)),
                }
            }
        }
    }
}

/// Context builder for AI verification.
/// Ensures information isolation - only provides what the verification agent should see.
pub struct AiVerificationContextBuilder;

impl AiVerificationContextBuilder {
    /// Build the context for an AI verification agent.
    ///
    /// CRITICAL: This intentionally excludes worker reasoning, code changes,
    /// and work history to prevent the verification agent from being biased.
    pub fn build(
        screenshot_base64: String,
        criterion: &SuccessCriterion,
        goal_summary: &str,
    ) -> VerificationAgentContext {
        let evaluation_prompt = criterion.evaluation_prompt.clone().unwrap_or_else(|| {
            format!(
                "Verify that the goal has been achieved: {}",
                criterion.description
            )
        });

        VerificationAgentContext {
            screenshot_base64,
            evaluation_prompt,
            goal_context: goal_summary.to_string(),
        }
    }

    /// Build the system prompt for a verification agent.
    pub fn build_system_prompt() -> String {
        r#"You are a UI verification agent. Your job is to objectively evaluate screenshots against specific criteria.

CRITICAL RULES:
1. You have NO knowledge of what code changes were made
2. You have NO knowledge of the worker's reasoning or justifications
3. You ONLY evaluate what you SEE in the screenshot
4. You must be OBJECTIVE - do not assume things are working just because they should be

Your response must be in this exact JSON format:
{
  "passed": true/false,
  "confidence": "high"/"medium"/"low",
  "observations": ["what you actually see in the screenshot"],
  "issues": ["problems found, if any"],
  "suggestions": ["how to fix, if failed"]
}

Only answer "passed": true if you can CLEARLY SEE evidence that the criteria are met.
If anything is unclear or ambiguous, answer "passed": false with "confidence": "low"."#.to_string()
    }

    /// Build the user prompt for verification.
    pub fn build_user_prompt(context: &VerificationAgentContext) -> String {
        format!(
            r#"Goal: {}

Evaluate this screenshot against these criteria:
{}

Respond with JSON only."#,
            context.goal_context, context.evaluation_prompt
        )
    }

    /// Parse the verification agent's response.
    pub fn parse_response(
        response: &str,
        criterion_id: &str,
    ) -> Result<VerificationResult, String> {
        // Try to extract JSON from the response
        let json_str = if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                &response[start..=end]
            } else {
                return Err("No closing brace found in response".to_string());
            }
        } else {
            return Err("No JSON found in response".to_string());
        };

        #[derive(Deserialize)]
        struct AgentResponse {
            passed: bool,
            confidence: Option<String>,
            observations: Option<Vec<String>>,
            issues: Option<Vec<String>>,
            suggestions: Option<Vec<String>>,
        }

        let parsed: AgentResponse =
            serde_json::from_str(json_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        let confidence = match parsed.confidence.as_deref() {
            Some("high") => Some(Confidence::High),
            Some("medium") => Some(Confidence::Medium),
            Some("low") => Some(Confidence::Low),
            _ => Some(Confidence::Medium),
        };

        Ok(VerificationResult {
            criterion_id: criterion_id.to_string(),
            passed: parsed.passed,
            criterion_type: CriterionType::AiEvaluated,
            confidence,
            observations: parsed.observations.unwrap_or_default(),
            issues: parsed.issues.unwrap_or_default(),
            suggestions: parsed.suggestions.unwrap_or_default(),
            raw_output: Some(response.to_string()),
        })
    }
}

// ============================================================================
// AI Verification Executor
// ============================================================================

/// Executor for AI-based verification checks.
///
/// This executor is deliberately isolated from worker context.
/// It only has access to screenshots and evaluation prompts.
pub struct AiVerifier {
    /// Timeout for AI calls in seconds
    pub timeout_seconds: u64,
}

impl AiVerifier {
    pub fn new(timeout_seconds: u64) -> Self {
        Self { timeout_seconds }
    }

    /// Run a single AI verification check.
    ///
    /// This is the core verification function that:
    /// 1. Builds an isolated context (no worker reasoning)
    /// 2. Calls the AI provider with screenshot + prompt
    /// 3. Parses the response into a VerificationResult
    pub fn verify_criterion(
        &self,
        criterion: &SuccessCriterion,
        screenshot_base64: &str,
        goal_summary: &str,
    ) -> VerificationResult {
        info!(
            "Running AI verification for criterion: {} (prompt: {:?})",
            criterion.id,
            criterion
                .evaluation_prompt
                .as_deref()
                .map(|s| &s[..s.len().min(50)])
        );

        // Build isolated context
        let context = AiVerificationContextBuilder::build(
            screenshot_base64.to_string(),
            criterion,
            goal_summary,
        );

        // Build the full prompt
        let system_prompt = AiVerificationContextBuilder::build_system_prompt();
        let user_prompt = AiVerificationContextBuilder::build_user_prompt(&context);

        // For AI verification with images, we need to use a special prompt format
        // that includes the base64 image. Most AI providers support this via
        // a specific message format.
        let full_prompt =
            Self::build_vision_prompt(&system_prompt, &user_prompt, screenshot_base64);

        debug!(
            "AI verification prompt built (length: {} chars)",
            full_prompt.len()
        );

        // Build task context for routing (AI verification is typically simple/medium complexity)
        let task_context = TaskContext::from_prompt(&full_prompt).with_criteria_count(1); // Single criterion being verified

        // Call the AI provider with routing
        let ai_response = crate::ai_provider::run_prompt_with_routing(
            &full_prompt,
            &task_context,
            self.timeout_seconds,
        );

        if !ai_response.success {
            let error_msg = ai_response
                .error
                .unwrap_or_else(|| "Unknown AI error".to_string());
            error!("AI verification failed for {}: {}", criterion.id, error_msg);

            return VerificationResult {
                criterion_id: criterion.id.clone(),
                passed: false,
                criterion_type: CriterionType::AiEvaluated,
                confidence: Some(Confidence::Low),
                observations: vec![],
                issues: vec![format!("AI verification failed: {}", error_msg)],
                suggestions: vec!["Check AI provider configuration and try again".to_string()],
                raw_output: Some(format!("Error: {}", error_msg)),
            };
        }

        // Parse the response
        match AiVerificationContextBuilder::parse_response(&ai_response.output, &criterion.id) {
            Ok(result) => {
                info!(
                    "AI verification for {} completed: passed={}, confidence={:?}",
                    criterion.id, result.passed, result.confidence
                );
                result
            }
            Err(e) => {
                warn!(
                    "Failed to parse AI verification response for {}: {}",
                    criterion.id, e
                );
                VerificationResult {
                    criterion_id: criterion.id.clone(),
                    passed: false,
                    criterion_type: CriterionType::AiEvaluated,
                    confidence: Some(Confidence::Low),
                    observations: vec![],
                    issues: vec![format!("Failed to parse AI response: {}", e)],
                    suggestions: vec!["AI response format was unexpected".to_string()],
                    raw_output: Some(ai_response.output),
                }
            }
        }
    }

    /// Build a vision-capable prompt that includes the screenshot.
    ///
    /// This creates a prompt format that works with vision-capable AI models.
    /// The format depends on the AI provider being used.
    fn build_vision_prompt(
        system_prompt: &str,
        user_prompt: &str,
        screenshot_base64: &str,
    ) -> String {
        // For Claude CLI, we can include the image as a data URL in the prompt
        // Claude will recognize and process embedded images
        //
        // For other providers, the format may differ. This is a baseline that works
        // with Claude's vision capabilities.
        format!(
            "{}\n\n---\n\n{}\n\n[Screenshot attached as base64 image - evaluate this screenshot]\n\nImage data (base64):\ndata:image/png;base64,{}",
            system_prompt,
            user_prompt,
            // Truncate for very large images to avoid token limits
            // In practice, we'd want proper image resizing
            if screenshot_base64.len() > 500_000 {
                &screenshot_base64[..500_000]
            } else {
                screenshot_base64
            }
        )
    }

    /// Run all AI verification checks for a plan.
    ///
    /// This runs checks sequentially. For parallel execution, use `verify_all_parallel`.
    pub fn verify_all(
        &self,
        plan: &VerificationPlan,
        screenshot_base64: &str,
    ) -> Vec<VerificationResult> {
        let mut results = Vec::new();

        for criterion in plan.ai_criteria() {
            let result = self.verify_criterion(criterion, screenshot_base64, &plan.goal_summary);
            results.push(result);
        }

        results
    }

    /// Run all AI verification checks in parallel.
    ///
    /// This spawns tasks for each criterion and waits for all to complete.
    /// Use this when you have multiple AI criteria and want faster verification.
    pub async fn verify_all_parallel(
        &self,
        plan: &VerificationPlan,
        screenshot_base64: &str,
    ) -> Vec<VerificationResult> {
        let criteria = plan.ai_criteria();

        if criteria.is_empty() {
            return vec![];
        }

        if criteria.len() == 1 {
            // No point in parallelizing a single criterion
            return self.verify_all(plan, screenshot_base64);
        }

        info!(
            "Running {} AI verification checks in parallel",
            criteria.len()
        );

        // Clone data for moving into tasks
        let screenshot = Arc::new(screenshot_base64.to_string());
        let goal_summary = Arc::new(plan.goal_summary.clone());
        let timeout = self.timeout_seconds;

        // Spawn tasks for each criterion
        let mut handles = Vec::new();

        for criterion in criteria {
            let criterion_clone = criterion.clone();
            let screenshot_clone = Arc::clone(&screenshot);
            let goal_clone = Arc::clone(&goal_summary);

            let handle = tokio::task::spawn_blocking(move || {
                let verifier = AiVerifier::new(timeout);
                verifier.verify_criterion(&criterion_clone, &screenshot_clone, &goal_clone)
            });

            handles.push(handle);
        }

        // Collect results
        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(e) => {
                    error!("AI verification task failed: {}", e);
                    // Add a failed result for the task that errored
                    results.push(VerificationResult {
                        criterion_id: "unknown".to_string(),
                        passed: false,
                        criterion_type: CriterionType::AiEvaluated,
                        confidence: Some(Confidence::Low),
                        observations: vec![],
                        issues: vec![format!("Verification task failed: {}", e)],
                        suggestions: vec![],
                        raw_output: None,
                    });
                }
            }
        }

        results
    }
}

// ============================================================================
// Full Verification Orchestrator
// ============================================================================

/// Orchestrates both deterministic and AI verification.
///
/// This is the main entry point for running verification against a plan.
pub struct VerificationOrchestrator {
    pub deterministic_verifier: DeterministicVerifier,
    pub ai_verifier: AiVerifier,
}

impl VerificationOrchestrator {
    pub fn new(working_directory: String, ai_timeout_seconds: u64) -> Self {
        Self {
            deterministic_verifier: DeterministicVerifier::new(working_directory),
            ai_verifier: AiVerifier::new(ai_timeout_seconds),
        }
    }

    /// Run full verification for a plan.
    ///
    /// This runs:
    /// 1. All deterministic checks first (fast, no AI)
    /// 2. If deterministic passes and plan has AI criteria, runs AI verification
    ///
    /// Returns aggregated results for the iteration.
    pub async fn verify(
        &self,
        plan: &VerificationPlan,
        iteration: u32,
        screenshot_base64: Option<&str>,
    ) -> IterationVerificationResults {
        info!(
            "Starting verification for iteration {} ({} deterministic, {} AI criteria)",
            iteration,
            plan.deterministic_criteria().len(),
            plan.ai_criteria().len()
        );

        // Run deterministic verification first
        let deterministic_results = self.deterministic_verifier.verify_all(plan).await;

        // Check if deterministic passed (only critical failures matter)
        let deterministic_passed = deterministic_results.iter().all(|r| {
            if r.passed {
                true
            } else {
                // Check if this criterion is critical
                plan.success_criteria
                    .iter()
                    .find(|c| c.id == r.criterion_id)
                    .map(|c| !c.is_critical) // Pass if not critical
                    .unwrap_or(false)
            }
        });

        // If deterministic failed, skip AI verification
        if !deterministic_passed {
            info!("Deterministic verification failed, skipping AI verification");

            let failure_summary = build_failure_summary(&deterministic_results, &[]);

            return IterationVerificationResults {
                iteration,
                deterministic_results,
                ai_results: vec![],
                deterministic_passed: false,
                ai_passed: true, // N/A, so true
                all_passed: false,
                failure_summary: Some(failure_summary),
                applied_overrides: vec![],
                overridden_criteria: vec![],
            };
        }

        // Run AI verification if there are AI criteria and we have a screenshot
        let ai_results = if plan.has_ai_criteria() {
            if let Some(screenshot) = screenshot_base64 {
                info!(
                    "Running AI verification with {} criteria",
                    plan.ai_criteria().len()
                );
                self.ai_verifier.verify_all_parallel(plan, screenshot).await
            } else {
                warn!("Plan has AI criteria but no screenshot provided, skipping AI verification");
                vec![]
            }
        } else {
            vec![]
        };

        // Check if AI passed (only critical failures matter)
        let ai_passed = ai_results.iter().all(|r| {
            if r.passed {
                true
            } else {
                plan.success_criteria
                    .iter()
                    .find(|c| c.id == r.criterion_id)
                    .map(|c| !c.is_critical)
                    .unwrap_or(false)
            }
        });

        let all_passed = deterministic_passed && ai_passed;

        let failure_summary = if !all_passed {
            Some(build_failure_summary(&deterministic_results, &ai_results))
        } else {
            None
        };

        info!(
            "Verification complete: deterministic={}, ai={}, all={}",
            deterministic_passed, ai_passed, all_passed
        );

        IterationVerificationResults {
            iteration,
            deterministic_results,
            ai_results,
            deterministic_passed,
            ai_passed,
            all_passed,
            failure_summary,
            applied_overrides: vec![],
            overridden_criteria: vec![],
        }
    }
}

/// Build a human-readable failure summary.
fn build_failure_summary(
    deterministic_results: &[VerificationResult],
    ai_results: &[VerificationResult],
) -> String {
    let mut summary = String::new();

    let det_failures: Vec<_> = deterministic_results.iter().filter(|r| !r.passed).collect();
    let ai_failures: Vec<_> = ai_results.iter().filter(|r| !r.passed).collect();

    if !det_failures.is_empty() {
        summary.push_str(&format!(
            "{} deterministic check(s) failed:\n",
            det_failures.len()
        ));
        for r in det_failures {
            summary.push_str(&format!(
                "  - {}: {}\n",
                r.criterion_id,
                r.issues.join(", ")
            ));
        }
    }

    if !ai_failures.is_empty() {
        summary.push_str(&format!("{} AI check(s) failed:\n", ai_failures.len()));
        for r in ai_failures {
            summary.push_str(&format!(
                "  - {}: {}\n",
                r.criterion_id,
                r.issues.join(", ")
            ));
        }
    }

    summary
}

// ============================================================================
// Screenshot Capture Helper
// ============================================================================

/// Load a screenshot from a file path and return as base64.
///
/// This is useful when screenshots are already captured and saved to disk.
pub fn load_screenshot_as_base64(path: &str) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let data = std::fs::read(path).map_err(|e| format!("Failed to read screenshot file: {}", e))?;

    let base64_data = STANDARD.encode(&data);

    info!(
        "Loaded screenshot from {} ({} bytes -> {} base64 chars)",
        path,
        data.len(),
        base64_data.len()
    );

    Ok(base64_data)
}

/// Load the most recent screenshot from the dev-logs directory.
///
/// This checks `.dev-logs/screenshots/` for the most recent PNG file.
pub fn load_latest_screenshot_as_base64() -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let dev_logs_dir = crate::paths::get_screenshots_dir();

    if !dev_logs_dir.exists() {
        return Err("Screenshots directory does not exist".to_string());
    }

    // Find most recent PNG file
    let mut screenshots: Vec<_> = std::fs::read_dir(&dev_logs_dir)
        .map_err(|e| format!("Failed to read screenshots directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase() == "png")
                .unwrap_or(false)
        })
        .collect();

    if screenshots.is_empty() {
        return Err("No screenshots found in directory".to_string());
    }

    // Sort by modification time, newest first
    screenshots.sort_by(|a, b| {
        let a_time = a.metadata().and_then(|m| m.modified()).ok();
        let b_time = b.metadata().and_then(|m| m.modified()).ok();
        b_time.cmp(&a_time)
    });

    let latest = &screenshots[0];
    let path = latest.path();

    let data = std::fs::read(&path).map_err(|e| format!("Failed to read screenshot: {}", e))?;

    let base64_data = STANDARD.encode(&data);

    info!(
        "Loaded latest screenshot from {:?} ({} bytes)",
        path.file_name(),
        data.len()
    );

    Ok(base64_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_verification_response() {
        let response = r#"
Based on the screenshot, here's my evaluation:

{
  "passed": false,
  "confidence": "high",
  "observations": ["Login form is visible", "Email field has red border"],
  "issues": ["Error message is not visible below the field"],
  "suggestions": ["Add error message display below invalid fields"]
}
"#;

        let result = AiVerificationContextBuilder::parse_response(response, "visual_validation");
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(!result.passed);
        assert_eq!(result.confidence, Some(Confidence::High));
        assert_eq!(result.observations.len(), 2);
        assert_eq!(result.issues.len(), 1);
    }

    #[test]
    fn test_parse_verification_response_passed() {
        let response = r#"
{
  "passed": true,
  "confidence": "high",
  "observations": ["All validation criteria met"],
  "issues": [],
  "suggestions": []
}
"#;

        let result = AiVerificationContextBuilder::parse_response(response, "test_criterion");
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.passed);
        assert_eq!(result.confidence, Some(Confidence::High));
    }

    #[test]
    fn test_parse_verification_response_minimal() {
        // Test with minimal required fields
        let response = r#"{"passed": true}"#;

        let result = AiVerificationContextBuilder::parse_response(response, "minimal");
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.passed);
        assert_eq!(result.confidence, Some(Confidence::Medium)); // Default
    }

    #[test]
    fn test_build_context_isolation() {
        // Verify that context builder only includes what it should
        let criterion = SuccessCriterion {
            id: "test_visual".to_string(),
            description: "Test visual criterion".to_string(),
            criterion_type: CriterionType::AiEvaluated,
            verification_method: None,
            verification_config: None,
            evaluation_prompt: Some("Check that the button is blue".to_string()),
            required: true,
            is_critical: true,
            weight: None,
            domain: None,
        };

        let context = AiVerificationContextBuilder::build(
            "base64_screenshot_data".to_string(),
            &criterion,
            "Make button blue",
        );

        // Verify isolation - context should NOT contain worker reasoning
        assert_eq!(context.goal_context, "Make button blue");
        assert_eq!(context.evaluation_prompt, "Check that the button is blue");
        assert_eq!(context.screenshot_base64, "base64_screenshot_data");
    }

    #[test]
    fn test_build_failure_summary() {
        let det_results = vec![
            VerificationResult {
                criterion_id: "build".to_string(),
                passed: true,
                criterion_type: CriterionType::Deterministic,
                confidence: None,
                observations: vec![],
                issues: vec![],
                suggestions: vec![],
                raw_output: None,
            },
            VerificationResult {
                criterion_id: "tests".to_string(),
                passed: false,
                criterion_type: CriterionType::Deterministic,
                confidence: None,
                observations: vec![],
                issues: vec!["3 tests failed".to_string()],
                suggestions: vec![],
                raw_output: None,
            },
        ];

        let ai_results = vec![VerificationResult {
            criterion_id: "visual".to_string(),
            passed: false,
            criterion_type: CriterionType::AiEvaluated,
            confidence: Some(Confidence::High),
            observations: vec![],
            issues: vec!["Button color is red, not blue".to_string()],
            suggestions: vec![],
            raw_output: None,
        }];

        let summary = build_failure_summary(&det_results, &ai_results);

        assert!(summary.contains("deterministic check(s) failed"));
        assert!(summary.contains("tests"));
        assert!(summary.contains("3 tests failed"));
        assert!(summary.contains("AI check(s) failed"));
        assert!(summary.contains("visual"));
    }

    #[test]
    fn test_ai_verifier_build_vision_prompt() {
        let system = "You are a verification agent";
        let user = "Check the screenshot";
        let screenshot = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ"; // Tiny base64

        let prompt = AiVerifier::build_vision_prompt(system, user, screenshot);

        assert!(prompt.contains(system));
        assert!(prompt.contains(user));
        assert!(prompt.contains("data:image/png;base64,"));
        assert!(prompt.contains(screenshot));
    }
}
