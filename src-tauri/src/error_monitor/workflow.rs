//! Error fix workflow generation.
//!
//! This module provides functionality to generate a unified workflow
//! that uses the debug agent to analyze and fix detected errors.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error_monitor::curator::{DebugContext, DebugContextCurator};
use crate::error_monitor::storage::ErrorEventStorage;
use crate::error_monitor::types::{ErrorQuery, ErrorStatus};
use rusqlite::Connection;

/// Configuration for generating an error fix workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorFixWorkflowConfig {
    /// Name for the generated workflow
    pub name: String,
    /// Maximum iterations for the debug agent
    pub max_iterations: u32,
    /// Whether to include warnings in the fix scope
    pub include_warnings: bool,
    /// Specific error IDs to focus on (if empty, fix all unresolved)
    pub error_ids: Vec<i64>,
    /// Task run ID to scope errors to
    pub task_run_id: Option<String>,
    /// Additional context to provide to the debug agent
    pub additional_context: Option<String>,
}

impl Default for ErrorFixWorkflowConfig {
    fn default() -> Self {
        Self {
            name: "Fix Application Errors".to_string(),
            max_iterations: 10,
            include_warnings: false,
            error_ids: vec![],
            task_run_id: None,
            additional_context: None,
        }
    }
}

/// Result of workflow generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedWorkflow {
    /// The unified workflow configuration as JSON
    pub workflow_json: Value,
    /// Human-readable description of what the workflow will do
    pub description: String,
    /// Number of errors targeted
    pub error_count: u32,
    /// Whether there are critical errors
    pub has_critical: bool,
}

/// Generator for error fix workflows.
pub struct ErrorFixWorkflowGenerator {
    config: ErrorFixWorkflowConfig,
}

impl ErrorFixWorkflowGenerator {
    /// Create a new generator with default configuration.
    pub fn new() -> Self {
        Self {
            config: ErrorFixWorkflowConfig::default(),
        }
    }

    /// Create a generator with custom configuration.
    pub fn with_config(config: ErrorFixWorkflowConfig) -> Self {
        Self { config }
    }

    /// Generate a unified workflow for fixing errors.
    pub fn generate(&self, conn: &Connection) -> Result<GeneratedWorkflow, String> {
        // Get the debug context
        let curator = DebugContextCurator::new();
        let context = curator.build_context(conn, self.config.task_run_id.as_deref())?;

        if context.total_count == 0 {
            return Err("No errors to fix".to_string());
        }

        // Collect all error IDs being targeted
        let targeted_error_ids: Vec<i64> = context
            .critical_errors
            .iter()
            .chain(context.errors.iter())
            .chain(context.warnings.iter())
            .map(|e| e.id)
            .collect();

        // Build the AI prompt with error context
        let ai_prompt = self.build_ai_prompt(&context, &curator);

        // Build verification criteria
        let verification_criteria = self.build_verification_criteria(&context);

        // Generate the unified workflow JSON
        let workflow = json!({
            "name": self.config.name,
            "description": format!("Auto-generated workflow to fix {} detected errors", context.total_count),
            "version": "1.0",

            // Setup steps: prepare the debug context
            "setup_steps": [
                {
                    "name": "Collect Error Context",
                    "type": "shell",
                    "command": "echo 'Error fix workflow started'",
                    "timeout_seconds": 5
                }
            ],

            // Verification steps: check if errors are resolved
            "verification_steps": verification_criteria,

            // Agentic steps: the debug agent fixes errors
            "agentic_steps": [
                {
                    "name": "Debug Agent - Fix Errors",
                    "type": "ai_analysis",
                    "config": {
                        "prompt": ai_prompt,
                        "model": "claude-sonnet-4-20250514",
                        "max_iterations": self.config.max_iterations,
                        "tools": ["read_file", "write_file", "run_command", "search_files"],
                        "context": {
                            "error_count": context.total_count,
                            "has_critical": !context.critical_errors.is_empty(),
                            "focus_areas": context.focus_areas
                        }
                    }
                }
            ],

            // Completion steps: summarize what was fixed
            "completion_steps": [
                {
                    "name": "Generate Fix Summary",
                    "type": "shell",
                    "command": "echo 'Error fix workflow completed'",
                    "timeout_seconds": 5
                }
            ],

            // Workflow settings
            "settings": {
                "max_agentic_iterations": self.config.max_iterations,
                "stop_on_verification_pass": true,
                "continue_on_step_failure": false
            },

            // Error IDs targeted by this workflow - will be resolved on successful completion
            "targeted_error_ids": targeted_error_ids
        });

        let description = self.generate_description(&context);

        Ok(GeneratedWorkflow {
            workflow_json: workflow,
            description,
            error_count: context.total_count,
            has_critical: !context.critical_errors.is_empty(),
        })
    }

    /// Build the AI prompt for the debug agent.
    fn build_ai_prompt(&self, context: &DebugContext, curator: &DebugContextCurator) -> String {
        let mut prompt = String::new();

        prompt.push_str("You are a debug agent tasked with fixing application errors.\n\n");

        // Add the formatted error context
        prompt.push_str(&curator.format_for_ai(context));

        prompt.push_str("\n## Your Task\n\n");
        prompt.push_str("1. Analyze the errors above and identify root causes\n");
        prompt.push_str("2. For each error, determine the fix needed\n");
        prompt.push_str("3. Implement the fixes by modifying the relevant files\n");
        prompt.push_str("4. Verify your fixes don't introduce new errors\n");
        prompt.push_str("5. Document what you changed and why\n\n");

        if !context.patterns.is_empty() {
            prompt.push_str("## Detected Patterns to Address\n\n");
            for pattern in &context.patterns {
                prompt.push_str(&format!("- {}\n", pattern.name));
                if let Some(ref cause) = pattern.suggested_cause {
                    prompt.push_str(&format!("  Possible cause: {}\n", cause));
                }
            }
            prompt.push('\n');
        }

        if let Some(ref additional) = self.config.additional_context {
            prompt.push_str("## Additional Context\n\n");
            prompt.push_str(additional);
            prompt.push_str("\n\n");
        }

        prompt.push_str("## Important Guidelines\n\n");
        prompt.push_str("- Focus on fixing the root cause, not just the symptoms\n");
        prompt.push_str("- Make minimal changes necessary to fix each issue\n");
        prompt.push_str("- Preserve existing functionality\n");
        prompt.push_str("- If an error cannot be fixed, document why\n");

        prompt
    }

    /// Build verification criteria for the workflow.
    fn build_verification_criteria(&self, context: &DebugContext) -> Vec<Value> {
        let mut criteria = Vec::new();

        // Primary criterion: no more critical or error-severity issues
        criteria.push(json!({
            "name": "No Critical Errors",
            "type": "error_check",
            "config": {
                "severity": ["critical", "error"],
                "max_count": 0,
                "description": "Application should have no critical or error-level issues"
            }
        }));

        // If we had specific files with errors, add file-specific checks
        let mut files_checked: Vec<String> = Vec::new();
        for error in context.critical_errors.iter().chain(context.errors.iter()) {
            if let Some(ref loc) = error.location {
                if !files_checked.contains(&loc) && files_checked.len() < 5 {
                    files_checked.push(loc.clone());
                    criteria.push(json!({
                        "name": format!("No errors in {}", loc),
                        "type": "log_check",
                        "config": {
                            "file_pattern": loc,
                            "error_pattern": "(?i)(error|exception|traceback)",
                            "should_not_match": true,
                            "description": format!("File {} should not produce errors", loc)
                        }
                    }));
                }
            }
        }

        // Add a general log check
        criteria.push(json!({
            "name": "Application Logs Clean",
            "type": "log_check",
            "config": {
                "check_recent_logs": true,
                "max_error_count": 0,
                "time_window_seconds": 60,
                "description": "No new errors should appear in application logs"
            }
        }));

        criteria
    }

    /// Generate a human-readable description of the workflow.
    fn generate_description(&self, context: &DebugContext) -> String {
        let mut desc = format!(
            "This workflow will attempt to fix {} detected error(s)",
            context.total_count
        );

        if !context.critical_errors.is_empty() {
            desc.push_str(&format!(
                ", including {} CRITICAL issue(s)",
                context.critical_errors.len()
            ));
        }

        if !context.focus_areas.is_empty() {
            desc.push_str(". Focus areas: ");
            desc.push_str(&context.focus_areas.join(", "));
        }

        desc.push('.');
        desc
    }

    /// Generate a quick-fix workflow for a single error.
    pub fn generate_for_single_error(
        &self,
        conn: &Connection,
        error_id: i64,
    ) -> Result<GeneratedWorkflow, String> {
        // Get the specific error
        let error = ErrorEventStorage::get_by_id(conn, error_id)?
            .ok_or_else(|| format!("Error {} not found", error_id))?;

        let ai_prompt = format!(
            r#"You are a debug agent. Fix the following error:

## Error Details
- **Type:** {}
- **Message:** {}
- **Source:** {}
- **Occurrences:** {}

{}

{}

## Your Task
1. Identify the root cause of this error
2. Implement the fix
3. Verify the fix works
4. Document what you changed

Focus on a minimal, targeted fix for this specific issue.
"#,
            error.error_type.as_deref().unwrap_or("Unknown"),
            error.message,
            error.log_source_name,
            error.occurrence_count,
            error
                .location
                .as_ref()
                .map(|l| format!(
                    "**Location:** {}:{}",
                    l.file_path,
                    l.line_number.unwrap_or(0)
                ))
                .unwrap_or_default(),
            error
                .stack_trace
                .as_ref()
                .map(|st| format!("**Stack Trace:**\n```\n{}\n```", st))
                .unwrap_or_default()
        );

        let workflow = json!({
            "name": format!("Fix: {}", error.error_type.as_deref().unwrap_or(&error.message[..30.min(error.message.len())])),
            "description": format!("Fix error: {}", error.message),
            "version": "1.0",

            "setup_steps": [],

            "verification_steps": [
                {
                    "name": "Error Resolved",
                    "type": "error_check",
                    "config": {
                        "error_id": error_id,
                        "should_be_resolved": true,
                        "description": "The targeted error should be resolved"
                    }
                }
            ],

            "agentic_steps": [
                {
                    "name": "Fix Single Error",
                    "type": "ai_analysis",
                    "config": {
                        "prompt": ai_prompt,
                        "model": "claude-sonnet-4-20250514",
                        "max_iterations": 5,
                        "tools": ["read_file", "write_file", "run_command", "search_files"],
                        "context": {
                            "error_id": error_id,
                            "error_type": error.error_type
                        }
                    }
                }
            ],

            "completion_steps": [],

            "settings": {
                "max_agentic_iterations": 5,
                "stop_on_verification_pass": true,
                // Error ID targeted by this workflow - will be resolved on successful completion
                "targeted_error_ids": [error_id]
            }
        });

        Ok(GeneratedWorkflow {
            workflow_json: workflow,
            description: format!("Fix error: {}", error.message),
            error_count: 1,
            has_critical: error.severity == crate::error_monitor::types::ErrorSeverity::Critical,
        })
    }
}

impl Default for ErrorFixWorkflowGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Tauri command to generate an error fix workflow.
#[tauri::command]
pub async fn generate_error_fix_workflow(
    db: tauri::State<'_, std::sync::Arc<crate::database::CheckpointDb>>,
    config: Option<ErrorFixWorkflowConfig>,
) -> Result<GeneratedWorkflow, String> {
    let db = db.inner().clone();
    let config = config.unwrap_or_default();

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let generator = ErrorFixWorkflowGenerator::with_config(config);
        generator.generate(&conn)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Tauri command to generate a workflow to fix a single error.
#[tauri::command]
pub async fn generate_single_error_fix_workflow(
    db: tauri::State<'_, std::sync::Arc<crate::database::CheckpointDb>>,
    error_id: i64,
) -> Result<GeneratedWorkflow, String> {
    let db = db.inner().clone();

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;
        let generator = ErrorFixWorkflowGenerator::new();
        generator.generate_for_single_error(&conn, error_id)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Check if there are fixable errors and return a summary.
#[tauri::command]
pub async fn check_fixable_errors(
    db: tauri::State<'_, std::sync::Arc<crate::database::CheckpointDb>>,
    task_run_id: Option<String>,
) -> Result<FixableErrorsSummary, String> {
    let db = db.inner().clone();

    tokio::task::spawn_blocking(move || {
        let conn = db.connection()?;

        let query = ErrorQuery {
            task_run_id,
            status: Some(vec![
                ErrorStatus::New,
                ErrorStatus::Acknowledged,
                ErrorStatus::InProgress,
                ErrorStatus::Promoted,
            ]),
            ..Default::default()
        };

        let errors = ErrorEventStorage::query(&conn, &query)?;

        let critical_count = errors
            .iter()
            .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Critical)
            .count();

        let error_count = errors
            .iter()
            .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Error)
            .count();

        let warning_count = errors
            .iter()
            .filter(|e| e.severity == crate::error_monitor::types::ErrorSeverity::Warning)
            .count();

        Ok(FixableErrorsSummary {
            total: errors.len() as u32,
            critical_count: critical_count as u32,
            error_count: error_count as u32,
            warning_count: warning_count as u32,
            can_generate_workflow: !errors.is_empty(),
            recommended_action: if critical_count > 0 {
                "Immediate fix recommended - critical errors present".to_string()
            } else if error_count > 0 {
                "Fix recommended - errors present".to_string()
            } else if warning_count > 0 {
                "Optional fix - only warnings present".to_string()
            } else {
                "No action needed".to_string()
            },
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Summary of fixable errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixableErrorsSummary {
    /// Total number of fixable errors
    pub total: u32,
    /// Number of critical errors
    pub critical_count: u32,
    /// Number of regular errors
    pub error_count: u32,
    /// Number of warnings
    pub warning_count: u32,
    /// Whether a fix workflow can be generated
    pub can_generate_workflow: bool,
    /// Recommended action
    pub recommended_action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ErrorFixWorkflowConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert!(!config.include_warnings);
    }

    #[test]
    fn test_generator_creation() {
        let generator = ErrorFixWorkflowGenerator::new();
        assert_eq!(generator.config.max_iterations, 10);

        let custom_config = ErrorFixWorkflowConfig {
            max_iterations: 5,
            ..Default::default()
        };
        let generator = ErrorFixWorkflowGenerator::with_config(custom_config);
        assert_eq!(generator.config.max_iterations, 5);
    }
}
