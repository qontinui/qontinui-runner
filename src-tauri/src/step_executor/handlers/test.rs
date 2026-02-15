//! Test Step Handler
//!
//! Handles test steps that execute verification tests (Playwright, Python, Vision, Repository).
//! Supports execution by test ID (database lookup) or inline repository test commands.

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::database::TestType as DbTestType;
use crate::step_executor::executor::ExecutionStepConfig;
use crate::test_executor::{
    execute_test, RepoTestConfig, TestCategory, TestDefinition, TestStatus,
    TestType as ExecutorTestType, VisionConfig,
};

/// Handler for test steps.
pub struct TestHandler;

#[async_trait]
impl StepHandler for TestHandler {
    fn step_type(&self) -> &'static str {
        "test"
    }

    fn display_name(&self) -> &'static str {
        "Test"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let step_name = step.name.as_deref().unwrap_or("Test");

        // Determine execution path
        if let Some(ref test_id) = step.test_id {
            // Path A: Execute test by ID (database lookup)
            self.execute_test_by_id(test_id, step_name, context).await
        } else if step.test_type.as_deref() == Some("repository") {
            // Path B: Execute repository test (shell command)
            self.execute_repository_test(step, step_name, context).await
        } else if step.check_command.is_some() || step.shell_command.is_some() {
            // Path C: Any test type with an inline command runs as repository test
            info!(
                "Test step '{}' has no test_id but has inline command, running as repository test (test_type={:?})",
                step_name,
                step.test_type
            );
            self.execute_repository_test(step, step_name, context).await
        } else {
            StepHandlerResult::failure("No test_id specified and no inline command provided")
        }
    }
}

impl TestHandler {
    /// Execute a test by ID from the database
    async fn execute_test_by_id(
        &self,
        test_id: &str,
        step_name: &str,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        info!("Executing test by ID: {}", test_id);

        // Emit action started event
        let action_id = format!("test-{}", uuid::Uuid::new_v4());
        let action_name = format!("TEST: {}", step_name);
        let metadata = json!({
            "test_id": test_id,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &action_name, metadata)
            .await;

        // Fetch test definition from database
        let db = &context.app_state.checkpoint_db;
        let verification_test = match db.get_verification_test(test_id) {
            Ok(Some(test)) => test,
            Ok(None) => {
                let error = format!("Test not found: {}", test_id);
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &action_name,
                        start_timestamp,
                        sequence,
                        &error,
                        None,
                    )
                    .await;
                return StepHandlerResult::failure(error);
            }
            Err(e) => {
                let error = format!("Failed to fetch test: {}", e);
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &action_name,
                        start_timestamp,
                        sequence,
                        &error,
                        None,
                    )
                    .await;
                return StepHandlerResult::failure(error);
            }
        };

        // Convert database test type to executor test type
        let test_type = Self::convert_test_type(&verification_test.test_type);

        // Parse vision config if present
        let vision_config: Option<VisionConfig> = verification_test
            .vision_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Parse repo test config if present
        let repo_test_config: Option<RepoTestConfig> = verification_test
            .repo_test_config
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Build TestDefinition
        // Note: is_critical is false here; blocking behavior is controlled by gate steps
        let test_def = TestDefinition {
            id: verification_test.id.clone(),
            name: verification_test.name.clone(),
            test_type,
            category: Self::parse_category(verification_test.category.as_deref()),
            playwright_code: verification_test.playwright_code.clone(),
            vision_config,
            python_code: verification_test.python_code.clone(),
            repo_test_config,
            timeout_seconds: verification_test.timeout_seconds.unwrap_or(30),
            is_critical: false,
            config: verification_test.config.clone(),
        };

        // Execute the test (synchronous call)
        let result = execute_test(&test_def);

        // Build result metadata
        let result_metadata = json!({
            "test_id": test_id,
            "status": format!("{:?}", result.status),
            "duration_ms": result.duration_ms,
            "assertions_passed": result.assertions_passed,
            "assertions_failed": result.assertions_failed,
        });

        // Emit completion event
        let success = matches!(result.status, TestStatus::Passed);
        if success {
            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &action_name,
                    start_timestamp,
                    sequence,
                    result_metadata.clone(),
                )
                .await;

            info!(
                "Test '{}' passed: {}ms, {}/{} assertions",
                step_name,
                result.duration_ms,
                result.assertions_passed,
                result.assertions_passed + result.assertions_failed
            );

            StepHandlerResult::success_with_data(json!({
                "test_id": test_id,
                "status": "passed",
                "duration_ms": result.duration_ms,
                "output": result.output,
                "assertions_passed": result.assertions_passed,
                "assertions_failed": result.assertions_failed,
            }))
        } else {
            let error_msg = result
                .error
                .clone()
                .unwrap_or_else(|| format!("Test {:?}", result.status));

            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &action_name,
                    start_timestamp,
                    sequence,
                    &error_msg,
                    Some(result_metadata),
                )
                .await;

            warn!("Test '{}' failed: {}", step_name, error_msg);

            StepHandlerResult::failure_with_data(
                error_msg,
                json!({
                    "test_id": test_id,
                    "status": format!("{:?}", result.status),
                    "duration_ms": result.duration_ms,
                    "output": result.output,
                    "assertions_passed": result.assertions_passed,
                    "assertions_failed": result.assertions_failed,
                    "structured_output": result.structured_output,
                }),
            )
        }
    }

    /// Execute a repository test (shell command based)
    async fn execute_repository_test(
        &self,
        step: &ExecutionStepConfig,
        step_name: &str,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Get command - check both fields for backwards compatibility
        let command = step
            .check_command
            .as_ref()
            .or(step.shell_command.as_ref())
            .cloned()
            .unwrap_or_else(|| "pytest".to_string());

        // Get working directory
        let working_dir = step
            .check_working_directory
            .as_ref()
            .or(step.shell_command_working_directory.as_ref())
            .cloned();

        info!(
            "Executing repository test '{}': command='{}', working_dir={:?}",
            step_name, command, working_dir
        );

        // Emit action started event
        let action_id = format!("repo-test-{}", uuid::Uuid::new_v4());
        let action_name = format!("REPO TEST: {}", step_name);
        let metadata = json!({
            "command": &command,
            "working_directory": &working_dir,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &action_name, metadata)
            .await;

        // Build repo test config
        // Note: ParseFormat is not exported so we use serde to construct with defaults
        let repo_config: RepoTestConfig = serde_json::from_value(json!({
            "command": &command,
            "working_directory": working_dir.as_deref().unwrap_or("${PROJECT_ROOT}")
        }))
        .unwrap_or_else(|_| {
            // Fallback: minimal config (other fields have serde defaults)
            serde_json::from_value(json!({
                "command": &command,
                "working_directory": "."
            }))
            .expect("Basic RepoTestConfig should parse")
        });

        // Build TestDefinition for repository test
        // Note: is_critical is false here; blocking behavior is controlled by gate steps
        let test_def = TestDefinition {
            id: format!("repo-{}", uuid::Uuid::new_v4()),
            name: step_name.to_string(),
            test_type: ExecutorTestType::RepositoryTest,
            category: TestCategory::Integration,
            playwright_code: None,
            vision_config: None,
            python_code: None,
            repo_test_config: Some(repo_config),
            timeout_seconds: step.timeout_seconds.unwrap_or(300) as u32,
            is_critical: false,
            config: json!({}),
        };

        // Execute the test
        let result = execute_test(&test_def);

        // Build result metadata
        let result_metadata = json!({
            "command": &command,
            "status": format!("{:?}", result.status),
            "duration_ms": result.duration_ms,
            "exit_code": result.exit_code,
        });

        let success = matches!(result.status, TestStatus::Passed);
        if success {
            context
                .event_emitter
                .emit_action_completed(
                    &action_id,
                    &action_name,
                    start_timestamp,
                    sequence,
                    result_metadata.clone(),
                )
                .await;

            info!(
                "Repository test '{}' passed: {}ms",
                step_name, result.duration_ms
            );

            StepHandlerResult::success_with_data(json!({
                "status": "passed",
                "duration_ms": result.duration_ms,
                "output": result.output,
                "individual_tests": result.individual_tests,
                "coverage": result.coverage,
            }))
        } else {
            let error_msg = result
                .error
                .clone()
                .unwrap_or_else(|| format!("Repository test {:?}", result.status));

            context
                .event_emitter
                .emit_action_failed(
                    &action_id,
                    &action_name,
                    start_timestamp,
                    sequence,
                    &error_msg,
                    Some(result_metadata),
                )
                .await;

            warn!("Repository test '{}' failed: {}", step_name, error_msg);

            StepHandlerResult::failure_with_data(
                error_msg,
                json!({
                    "status": format!("{:?}", result.status),
                    "duration_ms": result.duration_ms,
                    "output": result.output,
                    "exit_code": result.exit_code,
                    "individual_tests": result.individual_tests,
                    "structured_output": result.structured_output,
                }),
            )
        }
    }

    /// Convert database TestType to test_executor TestType
    fn convert_test_type(db_type: &DbTestType) -> ExecutorTestType {
        match db_type {
            DbTestType::PlaywrightCdp => ExecutorTestType::PlaywrightCdp,
            DbTestType::QontinuiVision => ExecutorTestType::QontinuiVision,
            DbTestType::PythonScript => ExecutorTestType::PythonScript,
            DbTestType::RepositoryTest => ExecutorTestType::RepositoryTest,
        }
    }

    /// Parse test category from string
    fn parse_category(category: Option<&str>) -> TestCategory {
        match category {
            Some("visual") => TestCategory::Visual,
            Some("dom") => TestCategory::Dom,
            Some("network") => TestCategory::Network,
            Some("data") => TestCategory::Data,
            Some("log") => TestCategory::Log,
            Some("layout") => TestCategory::Layout,
            Some("unit") => TestCategory::Unit,
            Some("integration") => TestCategory::Integration,
            _ => TestCategory::Custom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handler_step_type() {
        let handler = TestHandler;
        assert_eq!(handler.step_type(), "test");
        assert_eq!(handler.display_name(), "Test");
    }

    #[test]
    fn test_convert_test_type() {
        assert!(matches!(
            TestHandler::convert_test_type(&DbTestType::PlaywrightCdp),
            ExecutorTestType::PlaywrightCdp
        ));
        assert!(matches!(
            TestHandler::convert_test_type(&DbTestType::PythonScript),
            ExecutorTestType::PythonScript
        ));
    }

    #[test]
    fn test_parse_category() {
        assert!(matches!(
            TestHandler::parse_category(Some("visual")),
            TestCategory::Visual
        ));
        assert!(matches!(
            TestHandler::parse_category(Some("integration")),
            TestCategory::Integration
        ));
        assert!(matches!(
            TestHandler::parse_category(None),
            TestCategory::Custom
        ));
    }
}
