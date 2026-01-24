//! Phase executors for the unified workflow.
//!
//! Each phase has a dedicated executor that handles a single responsibility:
//! - SetupExecutor: Runs one-time setup steps
//! - VerificationExecutor: Runs verification/test steps and reports results
//! - AgenticExecutor: Runs the AI with failure context
//! - CompletionExecutor: Runs completion steps (only if verification passed)

use std::sync::Arc;
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult, StepExecutor, VerificationPhaseResult};
use crate::AppState;

use super::types::{AgenticOutcome, LoopConfig};

// =============================================================================
// Setup Phase Executor
// =============================================================================

/// Executes the setup phase (runs once at the start).
///
/// Handles both automation steps (shell commands, workflows) and prompt steps (AI tasks).
pub struct SetupExecutor {
    executor: StepExecutor,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl SetupExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(
                app_state,
                config_storage,
                app_handle.clone(),
            ),
            app_handle,
            pid_tracker,
        }
    }

    /// Run setup steps. Returns true if successful.
    ///
    /// Executes automation steps first (shell commands, etc.), then prompt steps (AI tasks).
    pub async fn execute(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Run automation setup steps first
        if !automation_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            let (result, _has_gui) = self
                .executor
                .execute_setup_phase(automation_steps, execution_id, &[])
                .await;

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("SETUP-PHASE: Automation steps failed");
                return (false, all_results);
            }
        }

        // Run prompt setup steps (AI tasks)
        if !prompt_steps.is_empty() {
            info!(
                "SETUP-PHASE: Running {} prompt steps (AI tasks)",
                prompt_steps.len()
            );

            let setup_prompt: String = prompt_steps
                .iter()
                .filter_map(|s| s.prompt_content.as_ref())
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            if !setup_prompt.is_empty() {
                let success = self
                    .run_setup_ai(&setup_prompt, execution_id, workflow_name, timeout_seconds)
                    .await;
                overall_success = overall_success && success;

                if !success {
                    warn!("SETUP-PHASE: AI prompt steps failed");
                }
            }
        }

        if automation_steps.is_empty() && prompt_steps.is_empty() {
            info!("SETUP-PHASE: No setup steps to execute");
        } else {
            info!(
                "SETUP-PHASE: Completed with success={}",
                overall_success
            );
        }

        (overall_success, all_results)
    }

    async fn run_setup_ai(
        &self,
        prompt: &str,
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> bool {
        let session_id = format!("{}-setup", execution_id);

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!("{} - Setup", workflow_name)),
            phase: Some("setup".to_string()),
        });

        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: execution_id.to_string(),
            session_num: 0, // Setup is session 0
        });

        info!(
            "SETUP-PHASE: Running setup AI (session: {})",
            session_id
        );

        let prompt_for_claude = prompt.to_string();
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id;

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout_seconds,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        match result {
            Ok(Ok((success, output, _))) => {
                info!(
                    "SETUP-PHASE: AI completed (success={}, output={} chars)",
                    success,
                    output.len()
                );
                success
            }
            Ok(Err(e)) => {
                error!("SETUP-PHASE: AI failed: {}", e);
                false
            }
            Err(e) => {
                error!("SETUP-PHASE: Task join error: {}", e);
                false
            }
        }
    }
}

// =============================================================================
// Verification Phase Executor
// =============================================================================

/// Executes verification steps and determines if they all pass.
pub struct VerificationExecutor {
    executor: StepExecutor,
}

impl VerificationExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(app_state, config_storage, app_handle),
        }
    }

    /// Run verification steps.
    ///
    /// Returns (verification_result, step_results)
    pub async fn execute(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        iteration: u32,
    ) -> (VerificationPhaseResult, Vec<StepExecutionResult>) {
        if steps.is_empty() {
            info!(
                "VERIFICATION-PHASE: No verification steps defined (iteration {})",
                iteration
            );
            // No verification steps = verification passes
            return (
                VerificationPhaseResult {
                    iteration,
                    all_passed: true,
                    critical_failure: false,
                    total_steps: 0,
                    passed_steps: 0,
                    failed_steps: 0,
                    skipped_steps: 0,
                    total_duration_ms: 0,
                    step_results: Vec::new(),
                },
                Vec::new(),
            );
        }

        info!(
            "VERIFICATION-PHASE: Running {} steps (iteration {})",
            steps.len(),
            iteration
        );

        let result = self
            .executor
            .execute_verification_steps(steps, execution_id, iteration)
            .await;

        info!(
            "VERIFICATION-PHASE: Iteration {} result: all_passed={}, critical_failure={}, passed={}/{}, failed={}",
            iteration,
            result.all_passed,
            result.critical_failure,
            result.passed_steps,
            result.total_steps,
            result.failed_steps
        );

        let step_results = result.step_results.clone();
        (result, step_results)
    }
}

// =============================================================================
// Agentic Phase Executor
// =============================================================================

/// Executes the AI agentic phase with failure context.
pub struct AgenticExecutor {
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl AgenticExecutor {
    pub fn new(
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            app_handle,
            pid_tracker,
        }
    }

    /// Run the AI with the given prompt and failure context.
    ///
    /// This calls Claude directly (no session system, no orchestrator).
    pub async fn execute(
        &self,
        config: &LoopConfig,
        iteration: u32,
        failure_context: &str,
        has_agentic_steps: bool,
    ) -> AgenticOutcome {
        if !has_agentic_steps {
            info!(
                "AGENTIC-PHASE: No agentic steps defined, skipping (iteration {})",
                iteration
            );
            return AgenticOutcome::Skipped;
        }

        // Build enhanced prompt with failure context
        let enhanced_prompt = if failure_context.is_empty() {
            warn!(
                "AGENTIC-PHASE: No failure context provided for iteration {} - AI won't know what to fix!",
                iteration
            );
            config.base_prompt.clone()
        } else {
            info!(
                "AGENTIC-PHASE: Building prompt with {} chars of failure context (iteration {})",
                failure_context.len(),
                iteration
            );
            format!(
                "{}\n\n---\n\nThe following verification checks FAILED. Please fix these issues:\n\n{}\n\nFix the issues above and ensure all checks pass.",
                config.base_prompt, failure_context
            )
        };

        let session_id = format!("{}-agentic-{}", config.execution_id, iteration);

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        // Create context for output grouping
        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!("{} - Iteration {}", config.workflow_name, iteration)),
            phase: Some(format!("agentic-{}", iteration)),
        });

        // Create finding context
        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: config.execution_id.clone(),
            session_num: iteration,
        });

        info!(
            "AGENTIC-PHASE: Running Claude directly for iteration {} (session: {})",
            iteration, session_id
        );

        // Run Claude directly - bypasses session system and orchestrator
        let prompt_for_claude = enhanced_prompt;
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id.clone();
        let timeout = config.timeout_seconds;

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        match result {
            Ok(Ok((success, output, _retry_state))) => {
                info!(
                    "AGENTIC-PHASE: Claude completed (success={}, output={} chars) for iteration {}",
                    success,
                    output.len(),
                    iteration
                );

                if success {
                    AgenticOutcome::Success { output }
                } else {
                    AgenticOutcome::Failed {
                        output: output.clone(),
                        error: "AI reported failure".to_string(),
                    }
                }
            }
            Ok(Err(e)) => {
                error!(
                    "AGENTIC-PHASE: Claude failed with error for iteration {}: {}",
                    iteration, e
                );
                AgenticOutcome::Error { error: e }
            }
            Err(e) => {
                error!(
                    "AGENTIC-PHASE: Task join error for iteration {}: {}",
                    iteration, e
                );
                AgenticOutcome::Error {
                    error: format!("Task join error: {}", e),
                }
            }
        }
    }
}

// =============================================================================
// Completion Phase Executor
// =============================================================================

/// Executes the completion phase (runs once after verification passes).
pub struct CompletionExecutor {
    executor: StepExecutor,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl CompletionExecutor {
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
        pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(
                app_state,
                config_storage,
                app_handle.clone(),
            ),
            app_handle,
            pid_tracker,
        }
    }

    /// Run completion steps.
    ///
    /// This should ONLY be called when verification has passed.
    pub async fn execute(
        &self,
        automation_steps: &[ExecutionStepConfig],
        prompt_steps: &[ExecutionStepConfig],
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> (bool, Vec<StepExecutionResult>) {
        let mut all_results = Vec::new();
        let mut overall_success = true;

        // Run automation completion steps
        if !automation_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} automation steps",
                automation_steps.len()
            );

            let result = self
                .executor
                .execute_completion_phase(automation_steps, execution_id, &[])
                .await;

            overall_success = overall_success && result.success;
            all_results.extend(result.steps);

            if !result.success {
                warn!("COMPLETION-PHASE: Automation steps failed");
            }
        }

        // Run prompt completion steps (AI summary)
        if !prompt_steps.is_empty() {
            info!(
                "COMPLETION-PHASE: Running {} prompt steps (AI summary)",
                prompt_steps.len()
            );

            let completion_prompt: String = prompt_steps
                .iter()
                .filter_map(|s| s.prompt_content.as_ref())
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            if !completion_prompt.is_empty() {
                let success = self
                    .run_completion_ai(&completion_prompt, execution_id, workflow_name, timeout_seconds)
                    .await;
                overall_success = overall_success && success;
            }
        }

        (overall_success, all_results)
    }

    async fn run_completion_ai(
        &self,
        prompt: &str,
        execution_id: &str,
        workflow_name: &str,
        timeout_seconds: u64,
    ) -> bool {
        let session_id = format!("{}-completion", execution_id);

        let workspace_root = crate::mcp_api::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let pid_tracker = self.pid_tracker.clone();
        let retry_config = crate::settings::get_ai_settings().retry;
        let app_handle = self.app_handle.clone();

        let session_ctx = Some(crate::mcp_api::AiOutputSessionContext {
            session_id: Some(session_id.clone()),
            session_name: Some(format!("{} - Completion", workflow_name)),
            phase: Some("completion".to_string()),
        });

        let finding_ctx = Some(crate::mcp_api::FindingContext {
            task_run_id: execution_id.to_string(),
            session_num: 999,
        });

        info!(
            "COMPLETION-PHASE: Running completion AI (session: {})",
            session_id
        );

        let prompt_for_claude = prompt.to_string();
        let workspace_for_claude = workspace_root;
        let sid_for_claude = session_id;

        let result = tokio::task::spawn_blocking(move || {
            crate::mcp_api::run_claude_session_with_retry(
                &workspace_for_claude,
                &prompt_for_claude,
                &sid_for_claude,
                &app_handle,
                timeout_seconds,
                session_ctx,
                finding_ctx,
                Some(pid_tracker),
                Some(&retry_config),
            )
        })
        .await;

        match result {
            Ok(Ok((success, output, _))) => {
                info!(
                    "COMPLETION-PHASE: AI completed (success={}, output={} chars)",
                    success,
                    output.len()
                );
                success
            }
            Ok(Err(e)) => {
                error!("COMPLETION-PHASE: AI failed: {}", e);
                false
            }
            Err(e) => {
                error!("COMPLETION-PHASE: Task join error: {}", e);
                false
            }
        }
    }
}
