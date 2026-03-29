//! Error fix workflow generation.
//!
//! This module provides functionality to generate a unified workflow
//! that uses the debug agent to analyze and fix detected errors.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error_monitor::curator::{CuratedError, DebugContext, DebugContextCurator};

use crate::error_monitor::storage::ErrorEventStorage;
use crate::error_monitor::types::{ErrorQuery, ErrorStatus};
use crate::str_utils::truncate_str;
use rusqlite::Connection;

/// Default maximum iterations for error fix workflows.
const DEFAULT_FIX_MAX_ITERATIONS: u32 = 10;

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
            max_iterations: DEFAULT_FIX_MAX_ITERATIONS,
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
            // "content" field is used for the prompt text (maps to prompt_content)
            "agentic_steps": [
                {
                    "name": "Debug Agent - Fix Errors",
                    "type": "prompt",
                    "content": ai_prompt
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

        let (spec_count, runtime_count) = DebugContextCurator::classify_errors(context);

        if spec_count > 0 && runtime_count == 0 {
            // ALL errors are spec failures
            prompt.push_str("You are a debug agent tasked with fixing UI Bridge spec verification failures.\n\n");
            prompt.push_str("IMPORTANT: These are UI Bridge spec assertion failures, NOT application runtime errors.\n");
            prompt.push_str("UI Bridge specs are defined in `.spec.uibridge.json` files and verify UI element state ");
            prompt.push_str("(existence, text content, visibility, attributes).\n\n");
            prompt.push_str(
                "Do NOT look for Playwright tests, Jest tests, or other test frameworks.\n",
            );
            prompt.push_str("Do NOT search for application crashes or runtime errors — the application may be running fine.\n\n");
        } else if spec_count > 0 && runtime_count > 0 {
            // MIXED
            prompt.push_str("You are a debug agent tasked with fixing application errors and UI Bridge spec failures.\n\n");
            prompt.push_str("IMPORTANT: The errors below include BOTH types:\n");
            prompt.push_str(
                "- **Application runtime errors** — fix the application code that produces them\n",
            );
            prompt.push_str("- **UI Bridge spec failures** (messages starting with `SPEC: `) — these are assertion failures ");
            prompt.push_str("from `.spec.uibridge.json` files, not application crashes\n\n");
        } else {
            // NO spec failures — original behavior
            prompt.push_str("You are a debug agent tasked with fixing application errors.\n\n");
            prompt.push_str("IMPORTANT: The errors below come from application runtime logs, NOT from test suites.\n");
            prompt.push_str("Do NOT look for or modify test files. Fix the actual application code that produces these errors.\n\n");
        }

        // Add the formatted error context
        prompt.push_str(&curator.format_for_ai(context));

        prompt.push_str("\n## Your Task\n\n");

        if spec_count > 0 && runtime_count == 0 {
            prompt.push_str(
                "1. Read the spec failure messages to understand which assertions failed\n",
            );
            prompt.push_str(
                "2. Find the `.spec.uibridge.json` file containing the spec definition\n",
            );
            prompt
                .push_str("3. Check the `assertions` array to understand the expected UI state\n");
            prompt.push_str("4. Determine if the **app code** needs fixing (UI not rendering correctly) or the **spec** needs updating (expectations are outdated)\n");
            prompt.push_str("5. Implement the fix and verify the assertions would now pass\n");
            prompt.push_str("6. Document what you changed and why\n\n");
        } else if spec_count > 0 {
            prompt.push_str("1. Analyze all errors above and identify root causes\n");
            prompt.push_str("2. For runtime errors: fix the application code that produces them\n");
            prompt.push_str("3. For spec failures (SPEC: ...): find the `.spec.uibridge.json` file, check assertions, and fix either the app code or the spec definition\n");
            prompt.push_str("4. Verify your fixes don't introduce new errors\n");
            prompt.push_str("5. Document what you changed and why\n\n");
        } else {
            prompt.push_str("1. Analyze the errors above and identify root causes\n");
            prompt.push_str("2. For each error, determine the fix needed\n");
            prompt.push_str("3. Implement the fixes by modifying the relevant files\n");
            prompt.push_str("4. Verify your fixes don't introduce new errors\n");
            prompt.push_str("5. Document what you changed and why\n\n");
        }

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
        prompt.push_str("- If an error cannot be fixed (e.g., infrastructure issue, missing external dependency, requires manual intervention), document why\n\n");

        prompt.push_str("## Signaling Unfixable Errors\n\n");
        prompt.push_str("If you determine that one or more errors CANNOT be fixed automatically, output the marker:\n\n");
        prompt.push_str("```\n[UNFIXABLE_ERRORS]\n```\n\n");
        prompt.push_str("This signals the system to exit the verification loop gracefully and proceed to the summary phase.\n");
        prompt.push_str("Use this when:\n");
        prompt.push_str("- The error requires external infrastructure changes (database migrations, server configuration)\n");
        prompt.push_str("- The error is caused by a missing external service or dependency\n");
        prompt.push_str("- The fix requires credentials or permissions you don't have\n");
        prompt.push_str("- The error is a known limitation that needs architectural changes\n");
        prompt.push_str("- Multiple attempts to fix have failed and you've identified the root cause as unfixable\n\n");
        prompt.push_str("Before outputting [UNFIXABLE_ERRORS], provide a clear explanation of:\n");
        prompt.push_str("1. Which errors are unfixable and why\n");
        prompt.push_str("2. What would be needed to fix them (for human follow-up)\n");
        prompt.push_str("3. Any partial fixes you were able to apply\n");

        prompt
    }

    /// Build verification criteria for the workflow.
    ///
    /// Creates error-specific verification steps that check if each targeted error
    /// is resolved. This provides precise verification that the AI actually fixed
    /// the specific errors, rather than just checking if logs are clean.
    fn build_verification_criteria(&self, context: &DebugContext) -> Vec<Value> {
        let mut criteria = Vec::new();

        // Create a verification step for each error to check if it's resolved
        // Critical errors first (highest priority)
        for error in &context.critical_errors {
            criteria.push(self.build_error_check(error, "critical"));
        }

        // Regular errors
        for error in &context.errors {
            criteria.push(self.build_error_check(error, "error"));
        }

        // Warnings (if included)
        if self.config.include_warnings {
            for error in &context.warnings {
                criteria.push(self.build_error_check(error, "warning"));
            }
        }

        // If no specific errors, fall back to generic log_watch
        if criteria.is_empty() {
            criteria.push(json!({
                "name": "Application Logs Clean",
                "type": "log_watch",
                "time_window_seconds": 60,
                "error_patterns": ["(?i)(error|exception|traceback|failed|panic)"]
            }));
        }

        criteria
    }

    /// Build an error_resolved verification step for a specific error.
    fn build_error_check(&self, error: &CuratedError, severity: &str) -> Value {
        // Create a pattern from the error message
        // Escape special characters but keep the core message
        let pattern = self.create_error_pattern(&error.message);

        // Create a descriptive name
        let name = if let Some(ref error_type) = error.error_type {
            format!("[{}] {} resolved", severity.to_uppercase(), error_type)
        } else {
            let short_msg = if error.message.len() > 40 {
                format!("{}...", truncate_str(&error.message, 40))
            } else {
                error.message.clone()
            };
            format!("[{}] '{}' resolved", severity.to_uppercase(), short_msg)
        };

        json!({
            "name": name,
            "type": "error_resolved",
            "error_id": error.id,
            "error_pattern": pattern,
            "error_source": error.source,
            "time_window_seconds": 120  // Longer window to catch lingering errors
        })
    }

    /// Create a pattern from an error message for matching.
    ///
    /// This extracts the key parts of the error message while being lenient
    /// about variable parts (line numbers, timestamps, etc.).
    fn create_error_pattern(&self, message: &str) -> String {
        // Take the first line if multi-line
        let first_line = message.lines().next().unwrap_or(message);

        // Truncate very long messages
        let pattern = if first_line.len() > 200 {
            truncate_str(first_line, 200)
        } else {
            first_line
        };

        pattern.to_string()
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

## If the Error Cannot Be Fixed

If you determine this error CANNOT be fixed automatically (e.g., requires infrastructure changes,
missing dependencies, or manual intervention), output:

```
[UNFIXABLE_ERRORS]
```

Before doing so, explain:
1. Why the error cannot be fixed automatically
2. What would be needed to fix it (for human follow-up)
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

        // Create an error pattern from the message (first line, truncated if needed)
        let error_pattern = {
            let first_line = error.message.lines().next().unwrap_or(&error.message);
            if first_line.len() > 200 {
                first_line[..200].to_string()
            } else {
                first_line.to_string()
            }
        };

        let workflow = json!({
            "name": format!("Fix: {}", error.error_type.as_deref().unwrap_or(&error.message[..30.min(error.message.len())])),
            "description": format!("Fix error: {}", error.message),
            "version": "1.0",

            "setup_steps": [],

            // Verification uses error_resolved to check if this specific error is fixed
            "verification_steps": [
                {
                    "name": format!("Error '{}' resolved", error.error_type.as_deref().unwrap_or("Unknown")),
                    "type": "error_resolved",
                    "error_id": error_id,
                    "error_pattern": error_pattern,
                    "error_source": error.log_source_name,
                    "time_window_seconds": 120
                }
            ],

            // "content" field is used for the prompt text (maps to prompt_content)
            "agentic_steps": [
                {
                    "name": "Fix Single Error",
                    "type": "prompt",
                    "content": ai_prompt
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
///
/// Note: The generator uses SQLite connection internally for complex curator queries.
#[tauri::command]
pub async fn generate_error_fix_workflow(
    db: tauri::State<'_, std::sync::Arc<crate::database::CheckpointDb>>,
    config: Option<ErrorFixWorkflowConfig>,
) -> Result<GeneratedWorkflow, String> {
    let db = db.inner().clone();
    let config = config.unwrap_or_default();

    // PG-primary: use PG error context to generate fix workflow
    let pg = crate::database::pg::PgDb::global();
    let errors = pg.get_unresolved_errors(config.task_run_id.as_deref(), 50).await?;
    if errors.is_empty() {
        return Err("No errors to fix".to_string());
    }
    let has_critical = errors.iter().any(|e| e["severity"].as_str() == Some("critical"));
    let error_count = errors.len() as u32;
    let targeted_error_ids: Vec<i64> = errors.iter()
        .filter_map(|e| e["id"].as_i64())
        .collect();
    let error_summary: Vec<String> = errors.iter()
        .map(|e| format!("[{}] {}", e["severity"].as_str().unwrap_or("unknown"), e["message"].as_str().unwrap_or("(no message)")))
        .collect();
    let ai_prompt = format!(
        "Fix the following application errors:\n\n{}\n\nDebug, identify root causes, and fix all errors.",
        error_summary.join("\n")
    );
    let description = format!("Fix {} errors ({} critical)", error_count, if has_critical { "has" } else { "no" });
    let workflow = json!({
        "name": config.name,
        "steps": [],
        "prompt": ai_prompt,
        "targeted_error_ids": targeted_error_ids,
    });
    Ok(GeneratedWorkflow {
        workflow_json: workflow,
        description,
        error_count,
        has_critical,
    })
}

/// Tauri command to generate a workflow to fix a single error.
///
/// Note: The generator uses SQLite connection internally for complex curator queries.
#[tauri::command]
pub async fn generate_single_error_fix_workflow(
    db: tauri::State<'_, std::sync::Arc<crate::database::CheckpointDb>>,
    error_id: i64,
) -> Result<GeneratedWorkflow, String> {
    let _db = db.inner().clone();

    // PG-primary: get single error and generate workflow
    let pg = crate::database::pg::PgDb::global();
    let errors = pg.get_unresolved_errors(None, 200).await?;
    let target_error = errors.iter().find(|e| e["id"].as_i64() == Some(error_id))
        .ok_or_else(|| format!("Error {} not found or already resolved", error_id))?;

    let severity = target_error["severity"].as_str().unwrap_or("error");
    let message = target_error["message"].as_str().unwrap_or("(no message)");
    let file_path = target_error["file_path"].as_str().unwrap_or("");

    let ai_prompt = format!(
        "Fix this {} error: {}\nFile: {}\n\nAnalyze the root cause and implement a fix.",
        severity, message, file_path
    );
    let workflow = json!({
        "name": format!("Fix Error #{}", error_id),
        "steps": [],
        "prompt": ai_prompt,
        "targeted_error_ids": [error_id],
    });
    Ok(GeneratedWorkflow {
        workflow_json: workflow,
        description: format!("Fix single error: {}", truncate_str(message, 100)),
        error_count: 1,
        has_critical: severity == "critical",
    })
}

/// Check if there are fixable errors and return a summary.
#[tauri::command]
pub async fn check_fixable_errors(
    app_state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
    task_run_id: Option<String>,
) -> Result<FixableErrorsSummary, String> {
    let statuses = ["new", "acknowledged", "in_progress", "promoted"];

    let pg_rows = app_state
        .pg_db
        .query_error_events(
            task_run_id.as_deref(),
            Some(&statuses),
            None,
            None,
            None,
            None,
        )
        .await?;

    let errors: Vec<crate::error_monitor::types::StoredErrorEvent> = pg_rows
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();

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
